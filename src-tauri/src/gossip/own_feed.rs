use crate::error::AppError;
use futures_lite::StreamExt;
use iroh_gossip::api::Event;
use proscenium_types::{Visibility, now_millis, short_id, user_feed_topic};
use tauri::Emitter;

impl super::GossipService {
    pub async fn stop_own_feed(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(handle) = inner.own_feed_handle.take() {
            handle.abort();
            inner.my_sender = None;
            log::info!("[gossip] stopped own feed topic");
        }
    }

    pub(super) async fn start_own_feed_unconditional(&self) -> Result<(), AppError> {
        let mut inner = self.inner.lock().await;
        if inner
            .own_feed_handle
            .as_ref()
            .is_some_and(|h| !h.is_finished())
        {
            return Ok(());
        }

        let my_id = self.identity.read().await.master_pubkey.clone();
        let topic = user_feed_topic(&my_id);
        log::info!("[gossip] starting own feed topic for {}", short_id(&my_id));

        let topic_handle = self.gossip.subscribe(topic, vec![]).await?;
        let (sender, receiver) = topic_handle.split();
        inner.my_sender = Some(sender);

        let storage = self.storage.clone();
        let endpoint = self.endpoint.clone();
        let app_handle = self.app_handle.clone();
        let handle = tokio::spawn(async move {
            log::info!("[gossip-own] listener started for own feed neighbors");
            let mut receiver = receiver;
            loop {
                match receiver.try_next().await {
                    Ok(Some(event)) => match &event {
                        Event::NeighborUp(endpoint_id) => {
                            let transport_id = endpoint_id.to_string();
                            log::info!(
                                "[gossip-own] new follower transport: {}",
                                short_id(&transport_id)
                            );

                            let pubkey =
                                match crate::peer::query_identity(&endpoint, *endpoint_id).await {
                                    Ok(identity) => {
                                        let master = identity.master_pubkey.clone();
                                        let _ = storage.cache_peer_identity(&identity).await;
                                        if let Some(profile) = &identity.profile {
                                            let _ = storage.save_profile(&master, profile).await;
                                        }
                                        log::info!(
                                            "[gossip-own] resolved follower {} -> master={}",
                                            short_id(&transport_id),
                                            short_id(&master),
                                        );
                                        master
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "[gossip-own] failed to resolve identity for {}: {e}",
                                            short_id(&transport_id),
                                        );
                                        transport_id.clone()
                                    }
                                };

                            let now = now_millis();
                            match storage.upsert_follower(&my_id, &pubkey, now).await {
                                Ok(is_new) => {
                                    let _ = app_handle.emit("follower-changed", &pubkey);
                                    if is_new {
                                        let _ = storage
                                            .insert_notification(
                                                "follower", &pubkey, None, None, now,
                                            )
                                            .await;
                                        let _ = app_handle.emit("new-follower", &pubkey);
                                        let _ = app_handle.emit("notification-received", ());
                                    }
                                }
                                Err(e) => {
                                    log::error!("[gossip-own] failed to store follower: {e}");
                                }
                            }

                            if storage.is_following(&my_id, &pubkey).await.unwrap_or(false) {
                                log::info!(
                                    "[gossip-own] followed peer {} came online",
                                    short_id(&pubkey),
                                );
                            }
                        }
                        Event::NeighborDown(endpoint_id) => {
                            let transport_id = endpoint_id.to_string();
                            log::info!("[gossip-own] follower left: {}", short_id(&transport_id));

                            let pubkey = storage
                                .get_master_pubkey_for_transport(&transport_id)
                                .await
                                .unwrap_or(transport_id);

                            if let Err(e) = storage.set_follower_offline(&my_id, &pubkey).await {
                                log::error!("[gossip-own] failed to update follower: {e}");
                            }
                            let _ = app_handle.emit("follower-changed", &pubkey);
                        }
                        _ => {}
                    },
                    Ok(None) => {
                        log::warn!("[gossip-own] own feed stream ended");
                        break;
                    }
                    Err(e) => {
                        log::error!("[gossip-own] own feed receiver error: {e}");
                        break;
                    }
                }
            }
            log::warn!("[gossip-own] own feed stream ended");
        });

        inner.own_feed_handle = Some(handle);
        Ok(())
    }

    pub async fn start_own_feed(&self) -> Result<(), AppError> {
        let visibility = self.my_visibility().await;
        if visibility != Visibility::Public {
            log::info!("[gossip] skipping own feed topic (visibility={visibility})");
            return Ok(());
        }
        self.start_own_feed_unconditional().await
    }
}
