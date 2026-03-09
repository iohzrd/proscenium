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
    validate_profile, verify_delete_interaction_signature, verify_delete_post_signature,
    verify_profile_signature,
};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Sent by gossip tasks when their stream dies, so the reconnect loop can restart them.
pub enum ReconnectRequest {
    OwnFeed { attempt: u32 },
    Follow { pubkey: String, attempt: u32 },
}

fn backoff_secs(attempt: u32) -> u64 {
    // 5, 10, 20, 40, 60, 60, ...
    (5 * 2u64.pow(attempt)).min(60)
}

pub struct FeedManager {
    pub gossip: Gossip,
    #[allow(dead_code)]
    pub endpoint: Endpoint,
    /// The permanent identity (master public key).
    pub master_pubkey: String,
    my_sender: Option<GossipSender>,
    own_feed_handle: Option<JoinHandle<()>>,
    pub subscriptions: HashMap<String, JoinHandle<()>>,
    pub storage: Arc<Storage>,
    pub app_handle: AppHandle,
    reconnect_tx: tokio::sync::mpsc::UnboundedSender<ReconnectRequest>,
}

impl FeedManager {
    pub fn new(
        gossip: Gossip,
        endpoint: Endpoint,
        master_pubkey: String,
        storage: Arc<Storage>,
        app_handle: AppHandle,
        reconnect_tx: tokio::sync::mpsc::UnboundedSender<ReconnectRequest>,
    ) -> Self {
        Self {
            gossip,
            endpoint,
            master_pubkey,
            my_sender: None,
            own_feed_handle: None,
            subscriptions: HashMap::new(),
            storage,
            app_handle,
            reconnect_tx,
        }
    }

    fn my_visibility(&self) -> Visibility {
        self.storage
            .get_visibility(&self.master_pubkey)
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
        if let Some(handle) = self.own_feed_handle.take() {
            handle.abort();
            self.my_sender = None;
            log::info!("[gossip] stopped own feed topic");
        }
    }

    async fn start_own_feed_unconditional(&mut self) -> anyhow::Result<()> {
        if self
            .own_feed_handle
            .as_ref()
            .is_some_and(|h| !h.is_finished())
        {
            return Ok(());
        }

        let my_id = self.master_pubkey.clone();
        let topic = user_feed_topic(&my_id);
        log::info!("[gossip] starting own feed topic for {}", short_id(&my_id));

        let topic_handle = self.gossip.subscribe(topic, vec![]).await?;
        let (sender, receiver) = topic_handle.split();
        self.my_sender = Some(sender);

        let storage = self.storage.clone();
        let app_handle = self.app_handle.clone();
        let reconnect_tx = self.reconnect_tx.clone();
        let handle = tokio::spawn(async move {
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
                        log::warn!("[gossip-own] own feed stream ended");
                        break;
                    }
                    Err(e) => {
                        log::error!("[gossip-own] own feed receiver error: {e}");
                        break;
                    }
                }
            }
            // Task died naturally -- request reconnection
            let _ = reconnect_tx.send(ReconnectRequest::OwnFeed { attempt: 0 });
        });

        self.own_feed_handle = Some(handle);
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

    pub async fn broadcast_delete(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> anyhow::Result<()> {
        if let Some(sender) = self.my_sender.as_ref() {
            let msg = GossipMessage::DeletePost {
                id: id.to_string(),
                author: author.to_string(),
                signature: signature.to_string(),
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

    pub async fn broadcast_delete_interaction(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> anyhow::Result<()> {
        if let Some(sender) = self.my_sender.as_ref() {
            let msg = GossipMessage::DeleteInteraction {
                id: id.to_string(),
                author: author.to_string(),
                signature: signature.to_string(),
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

    /// Subscribe to a user's gossip feed topic.
    /// `pubkey` is the master pubkey (permanent identity).
    /// `transport_node_ids` are the transport NodeIds to use as gossip bootstrap peers.
    pub async fn follow_user(
        &mut self,
        pubkey: String,
        transport_node_ids: &[String],
    ) -> anyhow::Result<()> {
        if self.subscriptions.contains_key(&pubkey) {
            log::info!("[gossip] already subscribed to {}", short_id(&pubkey));
            return Ok(());
        }

        let topic = user_feed_topic(&pubkey);
        let bootstrap: Vec<EndpointId> = transport_node_ids
            .iter()
            .filter_map(|id| id.parse().ok())
            .collect();

        if bootstrap.is_empty() {
            anyhow::bail!(
                "no valid transport NodeIds for gossip bootstrap of {}",
                short_id(&pubkey)
            );
        }

        log::info!(
            "[gossip] subscribing to {} with {} bootstrap nodes (topic: {})",
            short_id(&pubkey),
            bootstrap.len(),
            &format!("{:?}", topic)[..12]
        );
        let topic_handle = self.gossip.subscribe(topic, bootstrap).await?;
        let (sender, receiver) = topic_handle.split();
        log::info!("[gossip] subscribed to {}", short_id(&pubkey));

        let storage = self.storage.clone();
        let pk = pubkey.clone();
        let my_id = self.master_pubkey.clone();
        let app_handle = self.app_handle.clone();
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
                            );
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
            // Task died naturally -- request reconnection
            let _ = reconnect_tx.send(ReconnectRequest::Follow {
                pubkey: pk,
                attempt: 0,
            });
        });

        self.subscriptions.insert(pubkey, handle);
        Ok(())
    }

    /// Resolve the signing public key for a peer from the delegation cache.
    /// Returns None if no delegation is cached (backward compat: skip verification).
    fn resolve_signer(storage: &Storage, master_pubkey: &str) -> Option<iroh::PublicKey> {
        match storage.get_peer_signing_pubkey(master_pubkey) {
            Ok(Some(signing_pubkey)) => match signing_pubkey.parse() {
                Ok(pk) => Some(pk),
                Err(e) => {
                    log::warn!(
                        "[gossip-rx] bad signing pubkey for {}: {e}",
                        short_id(master_pubkey)
                    );
                    None
                }
            },
            _ => None,
        }
    }

    fn handle_follow_message(
        storage: &Storage,
        pk: &str,
        my_id: &str,
        app_handle: &AppHandle,
        content: &Bytes,
    ) {
        log::info!(
            "[gossip-rx] received {} bytes from {}",
            content.len(),
            short_id(pk)
        );
        match serde_json::from_slice(content) {
            Ok(GossipMessage::NewPost(post)) => {
                if post.author != pk {
                    log::info!(
                        "[gossip-rx] ignored post from {} (expected {})",
                        short_id(&post.author),
                        short_id(pk)
                    );
                } else if storage.is_hidden(pk).unwrap_or(false) {
                    log::info!(
                        "[gossip-rx] skipping post from muted/blocked {}",
                        short_id(pk)
                    );
                } else if process_incoming_post(storage, &post, "gossip-rx", my_id, app_handle) {
                    let _ = app_handle.emit("feed-updated", ());
                }
            }
            Ok(GossipMessage::DeletePost {
                id,
                author,
                signature,
            }) => {
                if author == pk {
                    // Verify delete signature
                    if let Some(signer) = Self::resolve_signer(storage, pk)
                        && let Err(reason) =
                            verify_delete_post_signature(&id, &author, &signature, &signer)
                    {
                        log::warn!(
                            "[gossip-rx] bad delete-post signature from {}: {reason}",
                            short_id(pk)
                        );
                        return;
                    }
                    match storage.get_post_by_id(&id) {
                        Ok(Some(post)) if post.author == pk => {
                            log::info!("[gossip-rx] delete post {id} from {}", short_id(pk));
                            if let Err(e) = storage.delete_post(&id) {
                                log::error!("[gossip-rx] failed to delete post: {e}");
                            }
                            let _ = app_handle.emit("feed-updated", ());
                        }
                        Ok(Some(_)) => {
                            log::error!("[gossip-rx] rejected delete for {id}: author mismatch");
                        }
                        Ok(None) => {}
                        Err(e) => {
                            log::error!("[gossip-rx] failed to look up post {id}: {e}");
                        }
                    }
                }
            }
            Ok(GossipMessage::ProfileUpdate(profile)) => {
                if let Err(reason) = validate_profile(&profile) {
                    log::error!(
                        "[gossip-rx] rejected profile from {}: {reason}",
                        short_id(pk)
                    );
                } else {
                    // Verify profile signature
                    if let Some(signer) = Self::resolve_signer(storage, pk)
                        && let Err(reason) = verify_profile_signature(&profile, &signer)
                    {
                        log::warn!(
                            "[gossip-rx] bad profile signature from {}: {reason}",
                            short_id(pk)
                        );
                        return;
                    }
                    log::info!(
                        "[gossip-rx] profile update from {}: {}",
                        short_id(pk),
                        profile.display_name
                    );
                    if let Err(e) = storage.save_profile(pk, &profile) {
                        log::error!("[gossip-rx] failed to store profile: {e}");
                    }
                    let _ = app_handle.emit("profile-updated", pk);
                }
            }
            Ok(GossipMessage::NewInteraction(interaction)) => {
                if interaction.author != pk {
                    // Ignore interactions not from the expected author
                } else if storage.is_hidden(pk).unwrap_or(false) {
                    log::info!(
                        "[gossip-rx] skipping interaction from muted/blocked {}",
                        short_id(pk)
                    );
                } else {
                    process_incoming_interaction(
                        storage,
                        &interaction,
                        pk,
                        "gossip-rx",
                        my_id,
                        app_handle,
                    );
                    let _ = app_handle.emit("interaction-received", &interaction);
                }
            }
            Ok(GossipMessage::DeleteInteraction {
                id,
                author,
                signature,
            }) => {
                if author == pk {
                    // Verify delete signature
                    if let Some(signer) = Self::resolve_signer(storage, pk)
                        && let Err(reason) =
                            verify_delete_interaction_signature(&id, &author, &signature, &signer)
                    {
                        log::warn!(
                            "[gossip-rx] bad delete-interaction signature from {}: {reason}",
                            short_id(pk)
                        );
                        return;
                    }
                    log::info!("[gossip-rx] delete interaction {id} from {}", short_id(pk));
                    if let Err(e) = storage.delete_interaction(&id, &author) {
                        log::error!("[gossip-rx] failed to delete interaction: {e}");
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

    pub fn unfollow_user(&mut self, pubkey: &str) {
        if let Some(handle) = self.subscriptions.remove(pubkey) {
            log::info!("[gossip] unsubscribed from {}", short_id(pubkey));
            handle.abort();
        }
    }
}

/// Reconnection loop: receives notifications from dead gossip tasks and restarts them.
pub async fn gossip_reconnect_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<ReconnectRequest>,
    feed: Arc<Mutex<FeedManager>>,
    tx: tokio::sync::mpsc::UnboundedSender<ReconnectRequest>,
) {
    while let Some(req) = rx.recv().await {
        match req {
            ReconnectRequest::OwnFeed { attempt } => {
                let delay = backoff_secs(attempt);
                log::info!("[reconnect] own feed died, restarting in {delay}s (attempt {attempt})");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                let mut fm = feed.lock().await;
                fm.own_feed_handle = None;
                fm.my_sender = None;
                if let Err(e) = fm.start_own_feed().await {
                    log::error!("[reconnect] own feed restart failed: {e}");
                    drop(fm);
                    let _ = tx.send(ReconnectRequest::OwnFeed {
                        attempt: attempt + 1,
                    });
                }
            }
            ReconnectRequest::Follow { pubkey, attempt } => {
                let delay = backoff_secs(attempt);
                log::info!(
                    "[reconnect] {} died, restarting in {delay}s (attempt {attempt})",
                    short_id(&pubkey)
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                let mut fm = feed.lock().await;
                fm.subscriptions.remove(&pubkey);
                let node_ids = fm
                    .storage
                    .get_peer_transport_node_ids(&pubkey)
                    .unwrap_or_default();
                if node_ids.is_empty() {
                    log::error!(
                        "[reconnect] no cached transport NodeIds for {}, cannot restart",
                        short_id(&pubkey)
                    );
                    drop(fm);
                    let _ = tx.send(ReconnectRequest::Follow {
                        pubkey,
                        attempt: attempt + 1,
                    });
                } else if let Err(e) = fm.follow_user(pubkey.clone(), &node_ids).await {
                    log::error!("[reconnect] {} restart failed: {e}", short_id(&pubkey));
                    drop(fm);
                    let _ = tx.send(ReconnectRequest::Follow {
                        pubkey,
                        attempt: attempt + 1,
                    });
                }
            }
        }
    }
}
