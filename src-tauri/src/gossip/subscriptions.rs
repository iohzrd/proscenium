use crate::error::AppError;
use futures_lite::StreamExt;
use iroh::EndpointId;
use iroh_gossip::api::Event;
use proscenium_types::{short_id, user_feed_topic};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

impl super::GossipService {
    /// Subscribe to a user's gossip feed topic.
    /// `pubkey` is the master pubkey (permanent identity).
    /// `transport_node_ids` are the transport NodeIds to use as gossip bootstrap peers.
    pub async fn follow_user(
        &self,
        pubkey: String,
        transport_node_ids: Vec<String>,
    ) -> Result<(), AppError> {
        let mut inner = self.inner.lock().await;

        // Skip if already subscribed with a live task; clean up finished handles.
        if let Some((handle, _)) = inner.subscriptions.get(&pubkey) {
            if !handle.is_finished() {
                log::info!("[gossip] already subscribed to {}", short_id(&pubkey));
                return Ok(());
            }
            inner.subscriptions.remove(&pubkey);
        }

        let topic = user_feed_topic(&pubkey);
        let bootstrap: Vec<EndpointId> = transport_node_ids
            .iter()
            .filter_map(|id| id.parse().ok())
            .collect();

        if bootstrap.is_empty() {
            return Err(AppError::Other(format!(
                "no valid transport NodeIds for gossip bootstrap of {}",
                short_id(&pubkey)
            )));
        }

        log::info!(
            "[gossip] subscribing to {} with {} bootstrap nodes",
            short_id(&pubkey),
            bootstrap.len(),
        );
        let topic_handle = self.gossip.subscribe(topic, bootstrap).await?;
        let (sender, receiver) = topic_handle.split();
        log::info!("[gossip] subscribed to {}", short_id(&pubkey));

        let storage = self.storage.clone();
        let pk = pubkey.clone();
        let my_id = self.identity.read().await.master_pubkey.clone();
        let app_handle = self.app_handle.clone();
        let has_neighbor = Arc::new(AtomicBool::new(false));
        let has_neighbor_task = has_neighbor.clone();
        let reconnect_tx = self.reconnect_tx.clone();

        let handle = tokio::spawn(async move {
            log::info!("[gossip-rx] listener started for {}", short_id(&pk));
            let _sender_hold = sender;
            let mut receiver = receiver;
            loop {
                match receiver.try_next().await {
                    Ok(Some(event)) => match &event {
                        Event::Received(msg) => {
                            Self::handle_follow_message(
                                &storage,
                                &pk,
                                &my_id,
                                &app_handle,
                                &msg.content,
                            )
                            .await;
                        }
                        Event::NeighborUp(_) => {
                            has_neighbor_task.store(true, Ordering::Relaxed);
                            log::info!("[gossip-rx] neighbor up for {}", short_id(&pk));
                        }
                        Event::NeighborDown(_) => {
                            log::info!("[gossip-rx] neighbor down for {}", short_id(&pk));
                        }
                        other => {
                            log::info!("[gossip-rx] event from {}: {other:?}", short_id(&pk));
                        }
                    },
                    Ok(None) => {
                        log::warn!("[gossip-rx] stream ended for {}", short_id(&pk));
                        break;
                    }
                    Err(e) => {
                        log::error!("[gossip-rx] receiver error for {}: {e}", short_id(&pk));
                        break;
                    }
                }
            }
            log::warn!(
                "[gossip-rx] stream for {} ended, requesting reconnect",
                short_id(&pk)
            );
            let _ = reconnect_tx.send(pk);
        });

        inner.subscriptions.insert(pubkey, (handle, has_neighbor));
        Ok(())
    }

    pub async fn unfollow_user(&self, pubkey: &str) {
        let mut inner = self.inner.lock().await;
        if let Some((handle, _)) = inner.subscriptions.remove(pubkey) {
            log::info!("[gossip] unsubscribed from {}", short_id(pubkey));
            handle.abort();
        }
    }

    pub async fn get_subscription_count(&self) -> usize {
        self.inner.lock().await.subscriptions.len()
    }
}
