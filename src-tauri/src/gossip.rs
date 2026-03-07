use crate::commands::sync::{process_incoming_interaction, process_incoming_post};
use crate::storage::Storage;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::{Endpoint, EndpointId};
use iroh_gossip::{
    Gossip,
    api::{Event, GossipSender},
};
use iroh_social_types::{
    GossipMessage, Interaction, Post, Profile, now_millis, short_id, user_feed_topic,
    validate_profile,
};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::task::JoinHandle;

pub struct FeedManager {
    pub gossip: Gossip,
    pub endpoint: Endpoint,
    pub my_sender: Option<GossipSender>,
    pub subscriptions: HashMap<String, (GossipSender, JoinHandle<()>)>,
    pub storage: Arc<Storage>,
    pub app_handle: AppHandle,
}

impl FeedManager {
    pub fn new(
        gossip: Gossip,
        endpoint: Endpoint,
        storage: Arc<Storage>,
        app_handle: AppHandle,
    ) -> Self {
        Self {
            gossip,
            endpoint,
            my_sender: None,
            subscriptions: HashMap::new(),
            storage,
            app_handle,
        }
    }

    pub async fn start_own_feed(&mut self) -> anyhow::Result<()> {
        let my_id = self.endpoint.id().to_string();
        let topic = user_feed_topic(&my_id);
        log::info!("[gossip] starting own feed topic for {}", short_id(&my_id));

        let topic_handle = self.gossip.subscribe(topic, vec![]).await?;
        let (sender, receiver) = topic_handle.split();
        self.my_sender = Some(sender);

        // Listen for neighbors joining/leaving our own feed topic (followers)
        let storage = self.storage.clone();
        let app_handle = self.app_handle.clone();
        tokio::spawn(async move {
            log::info!("[gossip-own] listener started for own feed neighbors");
            let mut receiver = receiver;
            loop {
                match receiver.try_next().await {
                    Ok(Some(event)) => match &event {
                        Event::NeighborUp(endpoint_id) => {
                            let pubkey = endpoint_id.to_string();
                            log::info!("[gossip-own] new follower: {}", short_id(&pubkey));
                            let now = now_millis();
                            match storage.upsert_follower(&pubkey, now) {
                                Ok(is_new) => {
                                    let _ = app_handle.emit("follower-changed", &pubkey);
                                    if is_new {
                                        let _ = storage.insert_notification(
                                            "follower", &pubkey, None, None, now,
                                        );
                                        let _ = app_handle.emit("new-follower", &pubkey);
                                        let _ = app_handle.emit("notification-received", ());
                                    }
                                }
                                Err(e) => {
                                    log::error!("[gossip-own] failed to store follower: {e}");
                                }
                            }
                        }
                        Event::NeighborDown(endpoint_id) => {
                            let pubkey = endpoint_id.to_string();
                            log::info!("[gossip-own] follower left: {}", short_id(&pubkey));
                            if let Err(e) = storage.set_follower_offline(&pubkey) {
                                log::error!("[gossip-own] failed to update follower: {e}");
                            }
                            let _ = app_handle.emit("follower-changed", &pubkey);
                        }
                        _ => {}
                    },
                    Ok(None) => {
                        log::info!("[gossip-own] own feed stream ended");
                        break;
                    }
                    Err(e) => {
                        log::error!("[gossip-own] own feed receiver error: {e}");
                        break;
                    }
                }
            }
            log::info!("[gossip-own] own feed listener stopped");
        });

        Ok(())
    }

    pub async fn broadcast_profile(&self, profile: &Profile) -> anyhow::Result<()> {
        let sender = self
            .my_sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("own feed not started"))?;

        let msg = GossipMessage::ProfileUpdate(profile.clone());
        let payload = serde_json::to_vec(&msg)?;
        sender.broadcast(Bytes::from(payload)).await?;
        log::info!("[gossip] broadcast profile: {}", profile.display_name);

        Ok(())
    }

    pub async fn broadcast_post(&self, post: &Post) -> anyhow::Result<()> {
        let sender = self
            .my_sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("own feed not started"))?;

        let msg = GossipMessage::NewPost(post.clone());
        let payload = serde_json::to_vec(&msg)?;
        sender.broadcast(Bytes::from(payload)).await?;
        log::info!("[gossip] broadcast post {}", &post.id);

        Ok(())
    }

    pub async fn broadcast_delete(&self, id: &str, author: &str) -> anyhow::Result<()> {
        let sender = self
            .my_sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("own feed not started"))?;

        let msg = GossipMessage::DeletePost {
            id: id.to_string(),
            author: author.to_string(),
        };
        let payload = serde_json::to_vec(&msg)?;
        sender.broadcast(Bytes::from(payload)).await?;
        log::info!("[gossip] broadcast delete {id}");

        Ok(())
    }

    pub async fn broadcast_interaction(&self, interaction: &Interaction) -> anyhow::Result<()> {
        let sender = self
            .my_sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("own feed not started"))?;

        let msg = GossipMessage::NewInteraction(interaction.clone());
        let payload = serde_json::to_vec(&msg)?;
        sender.broadcast(Bytes::from(payload)).await?;
        log::info!(
            "[gossip] broadcast {:?} on post {}",
            interaction.kind,
            &interaction.target_post_id
        );

        Ok(())
    }

    pub async fn broadcast_delete_interaction(&self, id: &str, author: &str) -> anyhow::Result<()> {
        let sender = self
            .my_sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("own feed not started"))?;

        let msg = GossipMessage::DeleteInteraction {
            id: id.to_string(),
            author: author.to_string(),
        };
        let payload = serde_json::to_vec(&msg)?;
        sender.broadcast(Bytes::from(payload)).await?;
        log::info!("[gossip] broadcast delete interaction {id}");

        Ok(())
    }

    pub async fn follow_user(&mut self, pubkey: String) -> anyhow::Result<()> {
        if self.subscriptions.contains_key(&pubkey) {
            log::info!("[gossip] already subscribed to {}", short_id(&pubkey));
            return Ok(());
        }

        let topic = user_feed_topic(&pubkey);
        let bootstrap: EndpointId = pubkey.parse().map_err(|e| anyhow::anyhow!("{e}"))?;

        log::info!(
            "[gossip] subscribing to {} (topic: {})",
            short_id(&pubkey),
            &format!("{:?}", topic)[..12]
        );
        let topic_handle = self.gossip.subscribe(topic, vec![bootstrap]).await?;
        let (sender, receiver) = topic_handle.split();
        log::info!("[gossip] subscribed to {}", short_id(&pubkey));

        let storage = self.storage.clone();
        let pk = pubkey.clone();
        let my_id = self.endpoint.id().to_string();
        let app_handle = self.app_handle.clone();
        let handle = tokio::spawn(async move {
            log::info!("[gossip-rx] listener started for {}", short_id(&pk));
            let mut receiver = receiver;
            loop {
                match receiver.try_next().await {
                    Ok(Some(event)) => match &event {
                        Event::Received(msg) => {
                            log::info!(
                                "[gossip-rx] received {} bytes from {}",
                                msg.content.len(),
                                short_id(&pk)
                            );
                            match serde_json::from_slice(&msg.content) {
                                Ok(GossipMessage::NewPost(post)) => {
                                    if post.author != pk {
                                        log::info!(
                                            "[gossip-rx] ignored post from {} (expected {})",
                                            short_id(&post.author),
                                            short_id(&pk)
                                        );
                                    } else if storage.is_hidden(&pk).unwrap_or(false) {
                                        log::info!(
                                            "[gossip-rx] skipping post from muted/blocked {}",
                                            short_id(&pk)
                                        );
                                    } else if process_incoming_post(
                                        &storage,
                                        &post,
                                        "gossip-rx",
                                        &my_id,
                                        &app_handle,
                                    ) {
                                        let _ = app_handle.emit("feed-updated", ());
                                    }
                                }
                                Ok(GossipMessage::DeletePost { id, author }) => {
                                    if author == pk {
                                        // Verify the stored post belongs to this author
                                        match storage.get_post_by_id(&id) {
                                            Ok(Some(post)) if post.author == pk => {
                                                log::info!(
                                                    "[gossip-rx] delete post {id} from {}",
                                                    short_id(&pk)
                                                );
                                                if let Err(e) = storage.delete_post(&id) {
                                                    log::error!(
                                                        "[gossip-rx] failed to delete post: {e}"
                                                    );
                                                }
                                                let _ = app_handle.emit("feed-updated", ());
                                            }
                                            Ok(Some(_)) => {
                                                log::error!(
                                                    "[gossip-rx] rejected delete for {id}: author mismatch"
                                                );
                                            }
                                            Ok(None) => {
                                                // Post not in our DB; ignore
                                            }
                                            Err(e) => {
                                                log::error!(
                                                    "[gossip-rx] failed to look up post {id}: {e}"
                                                );
                                            }
                                        }
                                    }
                                }
                                Ok(GossipMessage::ProfileUpdate(profile)) => {
                                    if let Err(reason) = validate_profile(&profile) {
                                        log::error!(
                                            "[gossip-rx] rejected profile from {}: {reason}",
                                            short_id(&pk)
                                        );
                                    } else {
                                        log::info!(
                                            "[gossip-rx] profile update from {}: {}",
                                            short_id(&pk),
                                            profile.display_name
                                        );
                                        if let Err(e) = storage.save_profile(&pk, &profile) {
                                            log::error!("[gossip-rx] failed to store profile: {e}");
                                        }
                                        let _ = app_handle.emit("profile-updated", &pk);
                                    }
                                }
                                Ok(GossipMessage::NewInteraction(interaction)) => {
                                    if interaction.author != pk {
                                        // Ignore interactions not from the expected author
                                    } else if storage.is_hidden(&pk).unwrap_or(false) {
                                        log::info!(
                                            "[gossip-rx] skipping interaction from muted/blocked {}",
                                            short_id(&pk)
                                        );
                                    } else {
                                        process_incoming_interaction(
                                            &storage,
                                            &interaction,
                                            &pk,
                                            "gossip-rx",
                                            &my_id,
                                            &app_handle,
                                        );
                                        let _ =
                                            app_handle.emit("interaction-received", &interaction);
                                    }
                                }
                                Ok(GossipMessage::DeleteInteraction { id, author }) => {
                                    if author == pk {
                                        log::info!(
                                            "[gossip-rx] delete interaction {id} from {}",
                                            short_id(&pk)
                                        );
                                        if let Err(e) = storage.delete_interaction(&id, &author) {
                                            log::error!(
                                                "[gossip-rx] failed to delete interaction: {e}"
                                            );
                                        }
                                        let _ = app_handle.emit(
                                            "interaction-deleted",
                                            serde_json::json!({ "id": id, "author": author }),
                                        );
                                    }
                                }
                                Err(e) => {
                                    log::error!("[gossip-rx] failed to parse message: {e}");
                                }
                            }
                        }
                        other => {
                            log::info!("[gossip-rx] event from {}: {other:?}", short_id(&pk));
                        }
                    },
                    Ok(None) => {
                        log::info!("[gossip-rx] stream ended for {}", short_id(&pk));
                        break;
                    }
                    Err(e) => {
                        log::error!("[gossip-rx] receiver error for {}: {e}", short_id(&pk));
                        break;
                    }
                }
            }
            log::info!("[gossip-rx] listener stopped for {}", short_id(&pk));
        });

        self.subscriptions.insert(pubkey, (sender, handle));
        Ok(())
    }

    pub fn unfollow_user(&mut self, pubkey: &str) {
        if let Some((_sender, handle)) = self.subscriptions.remove(pubkey) {
            log::info!("[gossip] unsubscribed from {}", short_id(pubkey));
            handle.abort();
        }
    }
}
