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
    GossipMessage, Interaction, Post, Profile, Visibility, now_millis, short_id, user_feed_topic,
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

    fn my_visibility(&self) -> Visibility {
        let my_id = self.endpoint.id().to_string();
        self.storage
            .get_visibility(&my_id)
            .unwrap_or(Visibility::Public)
    }

    /// Get the list of recipients for direct push based on visibility.
    /// Listed: all followers. Private: only mutuals.
    fn push_recipients(&self) -> Vec<String> {
        let visibility = self.my_visibility();
        match visibility {
            Visibility::Public => vec![],
            Visibility::Listed => self
                .storage
                .get_followers()
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.pubkey)
                .collect(),
            Visibility::Private => {
                let followers = self.storage.get_followers().unwrap_or_default();
                let follows = self.storage.get_follows().unwrap_or_default();
                let follow_set: std::collections::HashSet<&str> =
                    follows.iter().map(|f| f.pubkey.as_str()).collect();
                followers
                    .into_iter()
                    .filter(|f| follow_set.contains(f.pubkey.as_str()))
                    .map(|f| f.pubkey)
                    .collect()
            }
        }
    }

    /// Handle a visibility transition. Must be called BEFORE saving the new
    /// visibility to the database so that `broadcast_profile` can still reach
    /// gossip subscribers when downgrading from Public.
    pub async fn handle_visibility_change(
        &mut self,
        old: Visibility,
        new: Visibility,
        profile: &Profile,
    ) -> anyhow::Result<()> {
        if old == new {
            return Ok(());
        }

        log::info!("[visibility] transitioning {old} -> {new}");

        match (old, new) {
            (Visibility::Public, _) => {
                // Broadcast profile update via gossip before stopping feed
                self.broadcast_profile(profile).await?;
                self.stop_own_feed();
            }
            (_, Visibility::Public) => {
                // Start gossip feed, then broadcast
                self.start_own_feed_unconditional().await?;
                self.broadcast_profile(profile).await?;
            }
            _ => {
                // Listed <-> Private: both use push, just broadcast profile via push
                // (push_recipients will reflect new visibility after DB save)
            }
        }

        Ok(())
    }

    fn stop_own_feed(&mut self) {
        if self.my_sender.take().is_some() {
            log::info!("[gossip] stopped own feed topic");
        }
    }

    async fn start_own_feed_unconditional(&mut self) -> anyhow::Result<()> {
        if self.my_sender.is_some() {
            return Ok(());
        }

        let my_id = self.endpoint.id().to_string();
        let topic = user_feed_topic(&my_id);
        log::info!("[gossip] starting own feed topic for {}", short_id(&my_id));

        let topic_handle = self.gossip.subscribe(topic, vec![]).await?;
        let (sender, receiver) = topic_handle.split();
        self.my_sender = Some(sender);

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

    pub async fn start_own_feed(&mut self) -> anyhow::Result<()> {
        let visibility = self.my_visibility();
        if visibility != Visibility::Public {
            log::info!("[gossip] skipping own feed topic (visibility={visibility})");
            return Ok(());
        }
        self.start_own_feed_unconditional().await
    }

    pub async fn broadcast_profile(&self, profile: &Profile) -> anyhow::Result<()> {
        if let Some(sender) = self.my_sender.as_ref() {
            let msg = GossipMessage::ProfileUpdate(profile.clone());
            let payload = serde_json::to_vec(&msg)?;
            sender.broadcast(Bytes::from(payload)).await?;
            log::info!("[gossip] broadcast profile: {}", profile.display_name);
        } else {
            let recipients = self.push_recipients();
            for recipient in &recipients {
                if let Err(e) = self.storage.enqueue_push_profile(recipient) {
                    log::error!(
                        "[push] failed to enqueue profile push for {}: {e}",
                        short_id(recipient)
                    );
                }
            }
            if recipients.is_empty() {
                log::info!("[push] no recipients for profile push");
            } else {
                log::info!(
                    "[push] enqueued profile push for {} recipients",
                    recipients.len()
                );
            }
        }
        Ok(())
    }

    pub async fn broadcast_post(&self, post: &Post) -> anyhow::Result<()> {
        if let Some(sender) = self.my_sender.as_ref() {
            let msg = GossipMessage::NewPost(post.clone());
            let payload = serde_json::to_vec(&msg)?;
            sender.broadcast(Bytes::from(payload)).await?;
            log::info!("[gossip] broadcast post {}", &post.id);
        } else {
            // Enqueue to push outbox for each recipient
            let recipients = self.push_recipients();
            for recipient in &recipients {
                if let Err(e) = self.storage.enqueue_push_post(recipient, &post.id) {
                    log::error!(
                        "[push] failed to enqueue post {} for {}: {e}",
                        &post.id,
                        short_id(recipient)
                    );
                }
            }
            log::info!(
                "[push] enqueued post {} for {} recipients",
                &post.id,
                recipients.len()
            );
        }
        Ok(())
    }

    pub async fn broadcast_delete(&self, id: &str, author: &str) -> anyhow::Result<()> {
        if let Some(sender) = self.my_sender.as_ref() {
            let msg = GossipMessage::DeletePost {
                id: id.to_string(),
                author: author.to_string(),
            };
            let payload = serde_json::to_vec(&msg)?;
            sender.broadcast(Bytes::from(payload)).await?;
            log::info!("[gossip] broadcast delete {id}");
        } else {
            // Deletes for push-based delivery: remove from outbox if pending
            log::info!("[push] delete post {id} (pending pushes will skip missing post)");
        }
        Ok(())
    }

    pub async fn broadcast_interaction(&self, interaction: &Interaction) -> anyhow::Result<()> {
        if let Some(sender) = self.my_sender.as_ref() {
            let msg = GossipMessage::NewInteraction(interaction.clone());
            let payload = serde_json::to_vec(&msg)?;
            sender.broadcast(Bytes::from(payload)).await?;
            log::info!(
                "[gossip] broadcast {:?} on post {}",
                interaction.kind,
                &interaction.target_post_id
            );
        } else {
            let recipients = self.push_recipients();
            for recipient in &recipients {
                if let Err(e) = self
                    .storage
                    .enqueue_push_interaction(recipient, &interaction.id)
                {
                    log::error!(
                        "[push] failed to enqueue interaction {} for {}: {e}",
                        &interaction.id,
                        short_id(recipient)
                    );
                }
            }
            log::info!(
                "[push] enqueued interaction {} for {} recipients",
                &interaction.id,
                recipients.len()
            );
        }
        Ok(())
    }

    pub async fn broadcast_delete_interaction(&self, id: &str, author: &str) -> anyhow::Result<()> {
        if let Some(sender) = self.my_sender.as_ref() {
            let msg = GossipMessage::DeleteInteraction {
                id: id.to_string(),
                author: author.to_string(),
            };
            let payload = serde_json::to_vec(&msg)?;
            sender.broadcast(Bytes::from(payload)).await?;
            log::info!("[gossip] broadcast delete interaction {id}");
        } else {
            log::info!(
                "[push] delete interaction {id} (pending pushes will skip missing interaction)"
            );
        }
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
