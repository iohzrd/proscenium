use crate::storage::Storage;
use crate::sync::{process_incoming_interaction, process_incoming_post};
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::{Endpoint, EndpointId};
use iroh_gossip::{
    Gossip,
    api::{Event, GossipSender},
};
use iroh_social_types::{
    GossipMessage, Interaction, LinkedDevicesAnnouncement, Post, Profile, SigningKeyRotation,
    Visibility, now_millis, short_id, user_feed_topic, validate_profile,
    verify_delete_interaction_signature, verify_delete_post_signature,
    verify_linked_devices_announcement, verify_profile_signature, verify_rotation,
};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Sent by gossip tasks when their stream dies, so the reconnect loop can restart them.
pub enum ReconnectRequest {
    OwnFeed {
        attempt: u32,
    },
    Follow {
        pubkey: String,
        attempt: u32,
    },
    /// A followed peer appeared as a neighbor on our own feed -- refresh our
    /// subscription to their feed so the gossip overlay is connected in both
    /// directions.  This handles the startup race where we subscribe to their
    /// topic before they are reachable, ending up with zero neighbors.
    RefreshFollow {
        pubkey: String,
    },
    /// Tear down and re-establish all gossip connections (own feed + every
    /// follow).  Triggered after detecting a sleep/wake cycle where the
    /// underlying network connections have gone stale.
    RefreshAll,
}

fn backoff_secs(attempt: u32) -> u64 {
    // 5, 10, 20, 40, 60, 60, ...
    (5 * 2u64.pow(attempt)).min(60)
}

/// Commands sent to the GossipActor via its mpsc channel.
#[allow(dead_code)]
pub enum GossipCommand {
    Follow {
        pubkey: String,
        node_ids: Vec<String>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    Unfollow {
        pubkey: String,
    },
    StartOwnFeed {
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    StopOwnFeed,
    HandleVisibilityChange {
        old: Visibility,
        new: Visibility,
        profile: Profile,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    BroadcastPost(Post),
    BroadcastInteraction(Interaction),
    BroadcastDelete {
        id: String,
        author: String,
        signature: String,
    },
    BroadcastDeleteInteraction {
        id: String,
        author: String,
        signature: String,
    },
    BroadcastHeartbeat,
    BroadcastProfile(Profile),
    BroadcastLinkedDevices(LinkedDevicesAnnouncement),
    BroadcastSigningKeyRotation(SigningKeyRotation),
    GetSubscriptionCount(oneshot::Sender<usize>),
    TeardownAll,
    /// Tear down all subscriptions and re-establish them (sleep/wake recovery).
    RefreshAll,
}

/// Thin, cloneable handle for sending commands to the GossipActor.
#[derive(Clone)]
pub struct GossipHandle {
    cmd_tx: mpsc::Sender<GossipCommand>,
}

impl GossipHandle {
    pub async fn follow_user(&self, pubkey: String, node_ids: Vec<String>) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(GossipCommand::Follow {
                pubkey,
                node_ids,
                reply: tx,
            })
            .await?;
        rx.await?
    }

    pub fn unfollow_user(&self, pubkey: &str) {
        let _ = self.cmd_tx.try_send(GossipCommand::Unfollow {
            pubkey: pubkey.to_string(),
        });
    }

    #[allow(dead_code)]
    pub async fn start_own_feed(&self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(GossipCommand::StartOwnFeed { reply: tx })
            .await?;
        rx.await?
    }

    #[allow(dead_code)]
    pub fn stop_own_feed(&self) {
        let _ = self.cmd_tx.try_send(GossipCommand::StopOwnFeed);
    }

    pub async fn handle_visibility_change(
        &self,
        old: Visibility,
        new: Visibility,
        profile: Profile,
    ) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(GossipCommand::HandleVisibilityChange {
                old,
                new,
                profile,
                reply: tx,
            })
            .await?;
        rx.await?
    }

    pub async fn broadcast_post(&self, post: &Post) -> anyhow::Result<()> {
        self.cmd_tx
            .send(GossipCommand::BroadcastPost(post.clone()))
            .await?;
        Ok(())
    }

    pub async fn broadcast_interaction(&self, interaction: &Interaction) -> anyhow::Result<()> {
        self.cmd_tx
            .send(GossipCommand::BroadcastInteraction(interaction.clone()))
            .await?;
        Ok(())
    }

    pub async fn broadcast_delete(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> anyhow::Result<()> {
        self.cmd_tx
            .send(GossipCommand::BroadcastDelete {
                id: id.to_string(),
                author: author.to_string(),
                signature: signature.to_string(),
            })
            .await?;
        Ok(())
    }

    pub async fn broadcast_delete_interaction(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> anyhow::Result<()> {
        self.cmd_tx
            .send(GossipCommand::BroadcastDeleteInteraction {
                id: id.to_string(),
                author: author.to_string(),
                signature: signature.to_string(),
            })
            .await?;
        Ok(())
    }

    pub async fn broadcast_heartbeat(&self) -> anyhow::Result<()> {
        self.cmd_tx.send(GossipCommand::BroadcastHeartbeat).await?;
        Ok(())
    }

    pub async fn broadcast_profile(&self, profile: &Profile) -> anyhow::Result<()> {
        self.cmd_tx
            .send(GossipCommand::BroadcastProfile(profile.clone()))
            .await?;
        Ok(())
    }

    pub async fn broadcast_linked_devices(
        &self,
        announcement: &LinkedDevicesAnnouncement,
    ) -> anyhow::Result<()> {
        self.cmd_tx
            .send(GossipCommand::BroadcastLinkedDevices(announcement.clone()))
            .await?;
        Ok(())
    }

    pub async fn broadcast_signing_key_rotation(
        &self,
        rotation: &SigningKeyRotation,
    ) -> anyhow::Result<()> {
        self.cmd_tx
            .send(GossipCommand::BroadcastSigningKeyRotation(rotation.clone()))
            .await?;
        Ok(())
    }

    pub async fn get_subscription_count(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(GossipCommand::GetSubscriptionCount(tx))
            .await
            .is_err()
        {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    #[allow(dead_code)]
    pub fn teardown_all(&self) {
        let _ = self.cmd_tx.try_send(GossipCommand::TeardownAll);
    }

    pub fn refresh_all(&self) {
        let _ = self.cmd_tx.try_send(GossipCommand::RefreshAll);
    }
}

pub struct GossipActor {
    pub gossip: Gossip,
    #[allow(dead_code)]
    pub endpoint: Endpoint,
    /// The permanent identity (master public key).
    pub master_pubkey: String,
    my_sender: Option<GossipSender>,
    own_feed_handle: Option<JoinHandle<()>>,
    /// Maps peer master pubkey to (task_handle, has_neighbor_flag).
    /// The flag is set to true when the subscription has received at least one NeighborUp event,
    /// meaning the gossip mesh is actually connected. Used to distinguish "alive but isolated"
    /// from "alive and connected" when deciding whether to refresh.
    pub subscriptions: HashMap<String, (JoinHandle<()>, Arc<AtomicBool>)>,
    pub storage: Arc<Storage>,
    pub app_handle: AppHandle,
    reconnect_tx: tokio::sync::mpsc::UnboundedSender<ReconnectRequest>,
}

impl GossipActor {
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

    async fn my_visibility(&self) -> Visibility {
        self.storage
            .get_visibility(&self.master_pubkey)
            .await
            .unwrap_or(Visibility::Public)
    }

    /// Get the list of recipients for direct push based on visibility.
    /// Listed: all followers. Private: only mutuals.
    async fn push_recipients(&self) -> Vec<String> {
        let visibility = self.my_visibility().await;
        match visibility {
            Visibility::Public => vec![],
            Visibility::Listed => self
                .storage
                .get_followers()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.pubkey)
                .collect(),
            Visibility::Private => {
                let followers = self.storage.get_followers().await.unwrap_or_default();
                let follows = self.storage.get_follows().await.unwrap_or_default();
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
        let endpoint = self.endpoint.clone();
        let app_handle = self.app_handle.clone();
        let reconnect_tx = self.reconnect_tx.clone();
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

                            // Resolve master pubkey from transport NodeId
                            let pubkey = match crate::peer::query_identity(&endpoint, *endpoint_id)
                                .await
                            {
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
                                        "[gossip-own] failed to resolve identity for {}: {e}, using transport id",
                                        short_id(&transport_id),
                                    );
                                    transport_id.clone()
                                }
                            };

                            let now = now_millis();
                            match storage.upsert_follower(&pubkey, now).await {
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

                            // If we follow this peer, refresh our subscription to
                            // their feed.  This fixes the startup race where we
                            // subscribed before they were reachable and ended up
                            // with zero gossip neighbors on their topic.
                            if storage.is_following(&pubkey).await.unwrap_or(false) {
                                log::info!(
                                    "[gossip-own] followed peer {} came online, requesting subscription refresh",
                                    short_id(&pubkey),
                                );
                                let _ = reconnect_tx.send(ReconnectRequest::RefreshFollow {
                                    pubkey: pubkey.clone(),
                                });
                            }
                        }
                        Event::NeighborDown(endpoint_id) => {
                            let transport_id = endpoint_id.to_string();
                            log::info!("[gossip-own] follower left: {}", short_id(&transport_id));

                            // Try to resolve to master pubkey from cache
                            let pubkey = storage
                                .get_master_pubkey_for_transport(&transport_id)
                                .await
                                .unwrap_or(transport_id);

                            if let Err(e) = storage.set_follower_offline(&pubkey).await {
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
        let visibility = self.my_visibility().await;
        if visibility != Visibility::Public {
            log::info!("[gossip] skipping own feed topic (visibility={visibility})");
            return Ok(());
        }
        self.start_own_feed_unconditional().await
    }

    /// Serialize and broadcast a gossip message if we have an active sender.
    /// Returns `true` if the message was broadcast, `false` if no sender is active.
    async fn broadcast_msg(&self, msg: &GossipMessage, label: &str) -> anyhow::Result<bool> {
        if let Some(sender) = self.my_sender.as_ref() {
            let payload = serde_json::to_vec(msg)?;
            sender.broadcast(Bytes::from(payload)).await?;
            log::info!("[gossip] broadcast {label}");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Enqueue a push-outbox entry for each follower when gossip is unavailable.
    async fn enqueue_push<F, Fut>(&self, label: &str, enqueue_fn: F)
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), anyhow::Error>>,
    {
        let recipients = self.push_recipients().await;
        for recipient in &recipients {
            if let Err(e) = enqueue_fn(recipient.clone()).await {
                log::error!(
                    "[push] failed to enqueue {label} for {}: {e}",
                    short_id(recipient)
                );
            }
        }
        if !recipients.is_empty() {
            log::info!(
                "[push] enqueued {label} for {} recipients",
                recipients.len()
            );
        }
    }

    pub async fn broadcast_profile(&self, profile: &Profile) -> anyhow::Result<()> {
        let msg = GossipMessage::ProfileUpdate(profile.clone());
        if !self
            .broadcast_msg(&msg, &format!("profile: {}", profile.display_name))
            .await?
        {
            let storage = self.storage.clone();
            self.enqueue_push("profile", |r| {
                let s = storage.clone();
                async move { s.enqueue_push_profile(&r).await }
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_post(&self, post: &Post) -> anyhow::Result<()> {
        let msg = GossipMessage::NewPost(post.clone());
        if !self
            .broadcast_msg(&msg, &format!("post {}", &post.id))
            .await?
        {
            let storage = self.storage.clone();
            let post_id = post.id.clone();
            self.enqueue_push(&format!("post {}", &post.id), |r| {
                let s = storage.clone();
                let pid = post_id.clone();
                async move { s.enqueue_push_post(&r, &pid).await }
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_delete(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> anyhow::Result<()> {
        let msg = GossipMessage::DeletePost {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        if !self.broadcast_msg(&msg, &format!("delete {id}")).await? {
            log::info!("[push] delete post {id} (pending pushes will skip missing post)");
        }
        Ok(())
    }

    pub async fn broadcast_interaction(&self, interaction: &Interaction) -> anyhow::Result<()> {
        let msg = GossipMessage::NewInteraction(interaction.clone());
        let label = format!(
            "{:?} on post {}",
            interaction.kind, &interaction.target_post_id
        );
        if !self.broadcast_msg(&msg, &label).await? {
            let storage = self.storage.clone();
            let iid = interaction.id.clone();
            self.enqueue_push(&format!("interaction {}", &interaction.id), |r| {
                let s = storage.clone();
                let id = iid.clone();
                async move { s.enqueue_push_interaction(&r, &id).await }
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_delete_interaction(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> anyhow::Result<()> {
        let msg = GossipMessage::DeleteInteraction {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        if !self
            .broadcast_msg(&msg, &format!("delete interaction {id}"))
            .await?
        {
            log::info!(
                "[push] delete interaction {id} (pending pushes will skip missing interaction)"
            );
        }
        Ok(())
    }

    pub async fn broadcast_linked_devices(
        &self,
        announcement: &iroh_social_types::LinkedDevicesAnnouncement,
    ) -> anyhow::Result<()> {
        let msg = GossipMessage::LinkedDevices(announcement.clone());
        self.broadcast_msg(
            &msg,
            &format!("device announcement v{}", announcement.version),
        )
        .await?;
        Ok(())
    }

    pub async fn broadcast_signing_key_rotation(
        &self,
        rotation: &iroh_social_types::SigningKeyRotation,
    ) -> anyhow::Result<()> {
        let msg = GossipMessage::SigningKeyRotation(rotation.clone());
        self.broadcast_msg(
            &msg,
            &format!("signing key rotation to index {}", rotation.new_key_index),
        )
        .await?;
        Ok(())
    }

    pub async fn broadcast_heartbeat(&self) -> anyhow::Result<()> {
        if let Some(sender) = self.my_sender.as_ref() {
            let payload = serde_json::to_vec(&GossipMessage::Heartbeat)?;
            sender.broadcast(Bytes::from(payload)).await?;
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
        let has_neighbor = Arc::new(AtomicBool::new(false));
        let has_neighbor_task = has_neighbor.clone();

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
                            log::info!("[gossip-rx] event from {}: {event:?}", short_id(&pk));
                        }
                        Event::NeighborDown(_) => {
                            log::info!("[gossip-rx] event from {}: {event:?}", short_id(&pk));
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

        self.subscriptions.insert(pubkey, (handle, has_neighbor));
        Ok(())
    }

    async fn handle_follow_message(
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
                } else if storage.is_hidden(pk).await.unwrap_or(false) {
                    log::info!(
                        "[gossip-rx] skipping post from muted/blocked {}",
                        short_id(pk)
                    );
                } else if process_incoming_post(storage, &post, "gossip-rx", my_id, app_handle)
                    .await
                {
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
                    if let Some(signer) = crate::sync::resolve_signer(storage, pk).await
                        && let Err(reason) =
                            verify_delete_post_signature(&id, &author, &signature, &signer)
                    {
                        log::warn!(
                            "[gossip-rx] bad delete-post signature from {}: {reason}",
                            short_id(pk)
                        );
                        return;
                    }
                    match storage.get_post_by_id(&id).await {
                        Ok(Some(post)) if post.author == pk => {
                            log::info!("[gossip-rx] delete post {id} from {}", short_id(pk));
                            if let Err(e) = storage.delete_post(&id).await {
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
                    if let Some(signer) = crate::sync::resolve_signer(storage, pk).await
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
                    if let Err(e) = storage.save_profile(pk, &profile).await {
                        log::error!("[gossip-rx] failed to store profile: {e}");
                    }
                    let _ = app_handle.emit("profile-updated", pk);
                }
            }
            Ok(GossipMessage::NewInteraction(interaction)) => {
                if interaction.author != pk {
                    // Ignore interactions not from the expected author
                } else if storage.is_hidden(pk).await.unwrap_or(false) {
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
                    )
                    .await;
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
                    if let Some(signer) = crate::sync::resolve_signer(storage, pk).await
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
                    if let Err(e) = storage.delete_interaction(&id, &author).await {
                        log::error!("[gossip-rx] failed to delete interaction: {e}");
                    }
                    let _ = app_handle.emit(
                        "interaction-deleted",
                        serde_json::json!({ "id": id, "author": author }),
                    );
                }
            }
            Ok(GossipMessage::LinkedDevices(announcement)) => {
                if announcement.master_pubkey != pk {
                    log::warn!(
                        "[gossip-rx] ignoring device announcement from {} (expected {})",
                        short_id(&announcement.master_pubkey),
                        short_id(pk)
                    );
                    return;
                }
                // Verify the announcement signature and delegation chain
                if let Err(reason) = verify_linked_devices_announcement(&announcement) {
                    log::warn!(
                        "[gossip-rx] bad device announcement from {}: {reason}",
                        short_id(pk)
                    );
                    return;
                }
                // Skip stale announcements
                let cached_version = storage
                    .get_peer_announcement_version(pk)
                    .await
                    .unwrap_or(None)
                    .unwrap_or(0);
                if announcement.version <= cached_version {
                    return;
                }
                log::info!(
                    "[gossip-rx] device announcement from {} v{} ({} devices)",
                    short_id(pk),
                    announcement.version,
                    announcement.devices.len()
                );
                if let Err(e) = storage
                    .cache_peer_device_announcement(pk, &announcement)
                    .await
                {
                    log::error!("[gossip-rx] failed to cache device announcement: {e}");
                }
            }
            Ok(GossipMessage::SigningKeyRotation(rotation)) => {
                if rotation.master_pubkey != pk {
                    log::warn!(
                        "[gossip-rx] ignoring key rotation from {} (expected {})",
                        short_id(&rotation.master_pubkey),
                        short_id(pk)
                    );
                    return;
                }
                // Verify the rotation signature and embedded delegation
                if let Err(reason) = verify_rotation(&rotation) {
                    log::warn!(
                        "[gossip-rx] bad key rotation from {}: {reason}",
                        short_id(pk)
                    );
                    return;
                }
                // Reject replay/downgrade: new key index must be higher than cached
                if let Ok(Some(cached_delegation)) = storage.get_peer_delegation(pk).await
                    && rotation.new_key_index <= cached_delegation.key_index
                {
                    log::warn!(
                        "[gossip-rx] stale key rotation from {} (index {} <= cached {})",
                        short_id(pk),
                        rotation.new_key_index,
                        cached_delegation.key_index
                    );
                    return;
                }
                log::info!(
                    "[gossip-rx] signing key rotation from {} to index {}",
                    short_id(pk),
                    rotation.new_key_index
                );
                // Update cached delegation with the new signing key
                let response = iroh_social_types::IdentityResponse {
                    master_pubkey: rotation.master_pubkey.clone(),
                    delegation: rotation.new_delegation.clone(),
                    transport_node_ids: storage
                        .get_peer_transport_node_ids(pk)
                        .await
                        .unwrap_or_default(),
                    profile: None,
                };
                if let Err(e) = storage.cache_peer_identity(&response).await {
                    log::error!("[gossip-rx] failed to cache rotated delegation: {e}");
                }
                // Note: signing key rotation does NOT invalidate DM sessions.
                // The DM key is derived independently from the master key and
                // only changes when explicitly rotated via dm_key_index.
            }
            Ok(GossipMessage::Heartbeat) => {
                // No-op: heartbeats keep the underlying QUIC connections
                // alive so they don't time out during idle periods.
            }
            Err(e) => {
                log::error!("[gossip-rx] failed to parse message: {e}");
            }
        }
    }

    /// Tear down own feed and all follow subscriptions so they can be
    /// re-established with fresh connections (e.g. after sleep/wake).
    pub fn teardown_all(&mut self) {
        self.stop_own_feed();
        let keys: Vec<String> = self.subscriptions.keys().cloned().collect();
        for key in &keys {
            if let Some((handle, _)) = self.subscriptions.remove(key) {
                handle.abort();
            }
        }
        log::info!(
            "[gossip] tore down own feed + {} follow subscriptions",
            keys.len()
        );
    }

    pub fn unfollow_user(&mut self, pubkey: &str) {
        if let Some((handle, _)) = self.subscriptions.remove(pubkey) {
            log::info!("[gossip] unsubscribed from {}", short_id(pubkey));
            handle.abort();
        }
    }

    /// Spawn the actor, returning a GossipHandle for callers.
    pub fn spawn(self) -> GossipHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        tokio::spawn(self.run(cmd_rx));
        GossipHandle { cmd_tx }
    }

    /// Actor event loop: processes commands and reconnection requests.
    async fn run(mut self, mut cmd_rx: mpsc::Receiver<GossipCommand>) {
        let (reconnect_tx, mut reconnect_rx) =
            tokio::sync::mpsc::unbounded_channel::<ReconnectRequest>();
        // Replace the reconnect_tx so spawned gossip tasks send to OUR channel
        self.reconnect_tx = reconnect_tx.clone();

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd).await,
                        None => {
                            log::info!("[gossip-actor] command channel closed, shutting down");
                            break;
                        }
                    }
                }
                Some(req) = reconnect_rx.recv() => {
                    self.handle_reconnect(req, &reconnect_tx).await;
                }
            }
        }
        self.teardown_all();
    }

    async fn handle_command(&mut self, cmd: GossipCommand) {
        match cmd {
            GossipCommand::Follow {
                pubkey,
                node_ids,
                reply,
            } => {
                let result = self.follow_user(pubkey, &node_ids).await;
                let _ = reply.send(result);
            }
            GossipCommand::Unfollow { pubkey } => {
                self.unfollow_user(&pubkey);
            }
            GossipCommand::StartOwnFeed { reply } => {
                let result = self.start_own_feed().await;
                let _ = reply.send(result);
            }
            GossipCommand::StopOwnFeed => {
                self.stop_own_feed();
            }
            GossipCommand::HandleVisibilityChange {
                old,
                new,
                profile,
                reply,
            } => {
                let result = self.handle_visibility_change(old, new, &profile).await;
                let _ = reply.send(result);
            }
            GossipCommand::BroadcastPost(post) => {
                if let Err(e) = self.broadcast_post(&post).await {
                    log::error!("[gossip-actor] broadcast post failed: {e}");
                }
            }
            GossipCommand::BroadcastInteraction(interaction) => {
                if let Err(e) = self.broadcast_interaction(&interaction).await {
                    log::error!("[gossip-actor] broadcast interaction failed: {e}");
                }
            }
            GossipCommand::BroadcastDelete {
                id,
                author,
                signature,
            } => {
                if let Err(e) = self.broadcast_delete(&id, &author, &signature).await {
                    log::error!("[gossip-actor] broadcast delete failed: {e}");
                }
            }
            GossipCommand::BroadcastDeleteInteraction {
                id,
                author,
                signature,
            } => {
                if let Err(e) = self
                    .broadcast_delete_interaction(&id, &author, &signature)
                    .await
                {
                    log::error!("[gossip-actor] broadcast delete interaction failed: {e}");
                }
            }
            GossipCommand::BroadcastHeartbeat => {
                if let Err(e) = self.broadcast_heartbeat().await {
                    log::error!("[heartbeat] broadcast failed: {e}");
                }
            }
            GossipCommand::BroadcastProfile(profile) => {
                if let Err(e) = self.broadcast_profile(&profile).await {
                    log::error!("[gossip-actor] broadcast profile failed: {e}");
                }
            }
            GossipCommand::BroadcastLinkedDevices(announcement) => {
                if let Err(e) = self.broadcast_linked_devices(&announcement).await {
                    log::error!("[gossip-actor] broadcast linked devices failed: {e}");
                }
            }
            GossipCommand::BroadcastSigningKeyRotation(rotation) => {
                if let Err(e) = self.broadcast_signing_key_rotation(&rotation).await {
                    log::error!("[gossip-actor] broadcast key rotation failed: {e}");
                }
            }
            GossipCommand::GetSubscriptionCount(reply) => {
                let _ = reply.send(self.subscriptions.len());
            }
            GossipCommand::TeardownAll => {
                self.teardown_all();
            }
            GossipCommand::RefreshAll => {
                self.handle_reconnect(ReconnectRequest::RefreshAll, &self.reconnect_tx.clone())
                    .await;
            }
        }
    }

    async fn handle_reconnect(
        &mut self,
        req: ReconnectRequest,
        reconnect_tx: &tokio::sync::mpsc::UnboundedSender<ReconnectRequest>,
    ) {
        match req {
            ReconnectRequest::OwnFeed { attempt } => {
                let delay = backoff_secs(attempt);
                log::info!("[reconnect] own feed died, restarting in {delay}s (attempt {attempt})");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                self.own_feed_handle = None;
                self.my_sender = None;
                if let Err(e) = self.start_own_feed().await {
                    log::error!("[reconnect] own feed restart failed: {e}");
                    let _ = reconnect_tx.send(ReconnectRequest::OwnFeed {
                        attempt: attempt + 1,
                    });
                }
            }
            ReconnectRequest::RefreshAll => {
                log::info!("[reconnect] refreshing all gossip connections");
                self.teardown_all();

                if let Err(e) = self.start_own_feed().await {
                    log::error!("[reconnect] own feed restart failed: {e}");
                }

                let follows = self.storage.get_follows().await.unwrap_or_default();
                for f in &follows {
                    let node_ids = self
                        .storage
                        .get_peer_transport_node_ids(&f.pubkey)
                        .await
                        .unwrap_or_default();
                    if node_ids.is_empty() {
                        continue;
                    }
                    if let Err(e) = self.follow_user(f.pubkey.clone(), &node_ids).await {
                        log::error!("[reconnect] re-follow {} failed: {e}", short_id(&f.pubkey));
                    }
                }
                log::info!("[reconnect] refreshed own feed + {} follows", follows.len());
            }
            ReconnectRequest::RefreshFollow { pubkey } => {
                if self
                    .subscriptions
                    .get(&pubkey)
                    .is_some_and(|(h, has_neighbor)| {
                        !h.is_finished() && has_neighbor.load(Ordering::Relaxed)
                    })
                {
                    log::info!(
                        "[reconnect] skipping refresh for {} (subscription alive with neighbors)",
                        short_id(&pubkey)
                    );
                    return;
                }
                if let Some((handle, _)) = self.subscriptions.remove(&pubkey) {
                    handle.abort();
                    log::info!(
                        "[reconnect] removed isolated/dead subscription for {}",
                        short_id(&pubkey)
                    );
                }
                let node_ids = self
                    .storage
                    .get_peer_transport_node_ids(&pubkey)
                    .await
                    .unwrap_or_default();
                if node_ids.is_empty() {
                    log::warn!(
                        "[reconnect] no transport NodeIds for {}, cannot refresh",
                        short_id(&pubkey)
                    );
                } else if let Err(e) = self.follow_user(pubkey.clone(), &node_ids).await {
                    log::error!(
                        "[reconnect] refresh follow for {} failed: {e}",
                        short_id(&pubkey)
                    );
                } else {
                    log::info!(
                        "[reconnect] refreshed follow subscription for {}",
                        short_id(&pubkey)
                    );
                }
            }
            ReconnectRequest::Follow { pubkey, attempt } => {
                let delay = backoff_secs(attempt);
                log::info!(
                    "[reconnect] {} died, restarting in {delay}s (attempt {attempt})",
                    short_id(&pubkey)
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                if let Some((handle, _)) = self.subscriptions.remove(&pubkey) {
                    handle.abort();
                }
                let node_ids = self
                    .storage
                    .get_peer_transport_node_ids(&pubkey)
                    .await
                    .unwrap_or_default();
                if node_ids.is_empty() {
                    log::error!(
                        "[reconnect] no cached transport NodeIds for {}, cannot restart",
                        short_id(&pubkey)
                    );
                    let _ = reconnect_tx.send(ReconnectRequest::Follow {
                        pubkey,
                        attempt: attempt + 1,
                    });
                } else if let Err(e) = self.follow_user(pubkey.clone(), &node_ids).await {
                    log::error!("[reconnect] {} restart failed: {e}", short_id(&pubkey));
                    let _ = reconnect_tx.send(ReconnectRequest::Follow {
                        pubkey,
                        attempt: attempt + 1,
                    });
                }
            }
        }
    }
}
