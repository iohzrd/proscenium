#[cfg(target_os = "android")]
use crate::constants::ANDROID_NET_INTERVAL;
use crate::constants::{GOSSIP_HEARTBEAT_INTERVAL, HEALTH_TICK_INTERVAL, WAKE_THRESHOLD};
use crate::error::AppError;
use crate::ingest::{process_incoming_interaction, process_incoming_post};
use crate::state::SharedIdentity;
use crate::storage::Storage;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::{Endpoint, EndpointId};
use iroh_gossip::{
    Gossip,
    api::{Event, GossipSender},
};
use proscenium_types::{
    GossipMessage, Interaction, LinkedDevicesAnnouncement, Post, Profile, PushMessage,
    SigningKeyRotation, StageTicket, Visibility, now_millis, short_id, user_feed_topic,
    validate_profile, verify_delete_interaction_signature, verify_delete_post_signature,
    verify_linked_devices_announcement, verify_profile_signature, verify_rotation,
};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

struct GossipInner {
    my_sender: Option<GossipSender>,
    own_feed_handle: Option<JoinHandle<()>>,
    /// Maps peer master pubkey to (task_handle, has_neighbor_flag).
    subscriptions: HashMap<String, (JoinHandle<()>, Arc<AtomicBool>)>,
    /// Receiver for reconnect signals — moved out by start_background.
    reconnect_rx: Option<mpsc::UnboundedReceiver<String>>,
}

/// Cloneable gossip service — direct async methods, no command channel.
#[derive(Clone)]
pub struct GossipService {
    gossip: Gossip,
    endpoint: Endpoint,
    identity: SharedIdentity,
    storage: Arc<Storage>,
    app_handle: AppHandle,
    inner: Arc<tokio::sync::Mutex<GossipInner>>,
    /// Sender for reconnect signals — cloned into each subscription task.
    reconnect_tx: mpsc::UnboundedSender<String>,
}

impl GossipService {
    pub fn new(
        gossip: Gossip,
        endpoint: Endpoint,
        identity: SharedIdentity,
        storage: Arc<Storage>,
        app_handle: AppHandle,
    ) -> Self {
        let (reconnect_tx, reconnect_rx) = mpsc::unbounded_channel();
        Self {
            gossip,
            endpoint,
            identity,
            storage,
            app_handle,
            reconnect_tx,
            inner: Arc::new(tokio::sync::Mutex::new(GossipInner {
                my_sender: None,
                own_feed_handle: None,
                subscriptions: HashMap::new(),
                reconnect_rx: Some(reconnect_rx),
            })),
        }
    }

    /// Return a clone of the underlying iroh-gossip handle for direct topic subscriptions
    /// (e.g., Stage control plane topics that are separate from user feed topics).
    pub fn gossip_handle(&self) -> Gossip {
        self.gossip.clone()
    }

    /// Subscribe to all current follows, then spawn the reconnect loop and
    /// network-health task. Must be called once after setup is complete.
    pub async fn start_background(&self, token: CancellationToken) {
        // Subscribe to all current follows.
        let my_id = self.identity.read().await.master_pubkey.clone();
        let follows = self.storage.get_follows(&my_id).await.unwrap_or_default();
        for f in follows {
            let node_ids = self
                .storage
                .get_peer_transport_node_ids(&f.pubkey)
                .await
                .unwrap_or_default();
            if node_ids.is_empty() {
                log::warn!(
                    "[gossip] no NodeIds for {}, skipping startup subscribe",
                    short_id(&f.pubkey)
                );
                continue;
            }
            if let Err(e) = self.follow_user(f.pubkey.clone(), node_ids).await {
                log::error!(
                    "[gossip] failed to subscribe to {} at startup: {e}",
                    short_id(&f.pubkey)
                );
            }
        }

        // Take the reconnect receiver and spawn the reconnect loop.
        let rx = self
            .inner
            .lock()
            .await
            .reconnect_rx
            .take()
            .expect("start_background called twice");
        let this = self.clone();
        let reconnect_token = token.child_token();
        tokio::spawn(async move {
            this.reconnect_loop(rx, reconnect_token).await;
        });

        // Spawn network-health task (sleep/wake detection + heartbeat).
        let this = self.clone();
        let health_token = token.child_token();
        tokio::spawn(async move {
            this.network_health_task(health_token).await;
        });

        // Android: periodic network_change() since the OS doesn't notify the QUIC stack.
        #[cfg(target_os = "android")]
        {
            let ep = self.endpoint.clone();
            let android_token = token.child_token();
            tokio::spawn(async move {
                ep.network_change().await;
                log::info!("[android-net] initial network_change() sent");
                loop {
                    tokio::select! {
                        _ = android_token.cancelled() => break,
                        _ = tokio::time::sleep(ANDROID_NET_INTERVAL) => {
                            ep.network_change().await;
                        }
                    }
                }
            });
        }
    }

    async fn reconnect_loop(
        &self,
        mut rx: mpsc::UnboundedReceiver<String>,
        token: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                Some(pubkey) = rx.recv() => {
                    // Brief backoff before reconnecting.
                    tokio::select! {
                        _ = token.cancelled() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                    }
                    let node_ids = self
                        .storage
                        .get_peer_transport_node_ids(&pubkey)
                        .await
                        .unwrap_or_default();
                    if node_ids.is_empty() {
                        log::warn!(
                            "[gossip-reconnect] no NodeIds for {}, skipping",
                            short_id(&pubkey)
                        );
                        continue;
                    }
                    log::info!("[gossip-reconnect] reconnecting to {}", short_id(&pubkey));
                    if let Err(e) = self.follow_user(pubkey.clone(), node_ids).await {
                        log::error!(
                            "[gossip-reconnect] failed to reconnect to {}: {e}",
                            short_id(&pubkey)
                        );
                    }
                }
            }
        }
    }

    async fn network_health_task(&self, token: CancellationToken) {
        let mut last_tick = std::time::Instant::now();
        let mut last_heartbeat = std::time::Instant::now();
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(HEALTH_TICK_INTERVAL) => {}
            }
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(last_tick);
            last_tick = now;

            if elapsed > WAKE_THRESHOLD {
                log::info!(
                    "[wake] sleep/wake detected ({:.0}s elapsed), refreshing network",
                    elapsed.as_secs_f64()
                );
                self.endpoint.network_change().await;
                self.refresh_all().await;
            }

            if last_heartbeat.elapsed() >= GOSSIP_HEARTBEAT_INTERVAL {
                last_heartbeat = std::time::Instant::now();
                if let Err(e) = self.broadcast_heartbeat().await {
                    log::error!("[heartbeat] broadcast failed: {e}");
                }
            }
        }
    }

    /// Re-queue reconnect for all subscriptions whose tasks have exited.
    pub async fn refresh_all(&self) {
        let inner = self.inner.lock().await;
        for (pubkey, (handle, _)) in &inner.subscriptions {
            if handle.is_finished() {
                let _ = self.reconnect_tx.send(pubkey.clone());
            }
        }
    }

    async fn my_visibility(&self) -> Visibility {
        self.storage
            .get_visibility(&self.identity.read().await.master_pubkey)
            .await
            .unwrap_or(Visibility::Public)
    }

    /// Get the list of recipients for direct push based on visibility.
    /// Listed: all followers. Private: only mutuals.
    async fn push_recipients(&self) -> Vec<String> {
        let my_id = self.identity.read().await.master_pubkey.clone();
        let visibility = self.my_visibility().await;
        match visibility {
            Visibility::Public => vec![],
            Visibility::Listed => self
                .storage
                .get_followers(&my_id)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.pubkey)
                .collect(),
            Visibility::Private => {
                let followers = self.storage.get_followers(&my_id).await.unwrap_or_default();
                let follows = self.storage.get_follows(&my_id).await.unwrap_or_default();
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

    /// Fire-and-forget: attempt direct push to all visibility-appropriate recipients.
    /// If a recipient is offline the message is dropped — sync is the reliable fallback.
    async fn attempt_push(&self, msg: PushMessage) {
        let recipients = self.push_recipients().await;
        if recipients.is_empty() {
            return;
        }
        let endpoint = self.endpoint.clone();
        let storage = self.storage.clone();
        tokio::spawn(async move {
            for recipient in &recipients {
                let node_ids = storage
                    .get_peer_transport_node_ids(recipient)
                    .await
                    .unwrap_or_default();
                let mut delivered = false;
                for node_id_str in &node_ids {
                    let Ok(target) = node_id_str.parse() else {
                        continue;
                    };
                    if crate::push::push_to_peer(&endpoint, target, &msg)
                        .await
                        .is_ok()
                    {
                        delivered = true;
                        break;
                    }
                }
                if !delivered {
                    log::debug!("[push] {} offline, will sync later", short_id(recipient));
                }
            }
        });
    }

    /// Handle a visibility transition. Must be called BEFORE saving the new
    /// visibility to the database so that `broadcast_profile` can still reach
    /// gossip subscribers when downgrading from Public.
    pub async fn handle_visibility_change(
        &self,
        old: Visibility,
        new: Visibility,
        profile: &Profile,
    ) -> Result<(), AppError> {
        if old == new {
            return Ok(());
        }

        log::info!("[visibility] transitioning {old} -> {new}");

        match (old, new) {
            (Visibility::Public, _) => {
                self.broadcast_profile(profile).await?;
                self.stop_own_feed().await;
            }
            (_, Visibility::Public) => {
                self.start_own_feed_unconditional().await?;
                self.broadcast_profile(profile).await?;
            }
            _ => {
                // Listed <-> Private: both use push
            }
        }

        Ok(())
    }

    pub async fn stop_own_feed(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(handle) = inner.own_feed_handle.take() {
            handle.abort();
            inner.my_sender = None;
            log::info!("[gossip] stopped own feed topic");
        }
    }

    async fn start_own_feed_unconditional(&self) -> Result<(), AppError> {
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

    /// Serialize and broadcast a gossip message if we have an active sender.
    /// Returns `true` if the message was broadcast, `false` if no sender is active.
    async fn broadcast_msg(&self, msg: &GossipMessage, label: &str) -> Result<bool, AppError> {
        let sender = self.inner.lock().await.my_sender.clone();
        if let Some(sender) = sender {
            let payload = serde_json::to_vec(msg)?;
            sender.broadcast(Bytes::from(payload)).await?;
            log::info!("[gossip] broadcast {label}");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn broadcast_heartbeat(&self) -> Result<(), AppError> {
        self.broadcast_msg(&GossipMessage::Heartbeat, "heartbeat")
            .await?;
        Ok(())
    }

    pub async fn broadcast_profile(&self, profile: &Profile) -> Result<(), AppError> {
        let msg = GossipMessage::ProfileUpdate(profile.clone());
        if !self
            .broadcast_msg(&msg, &format!("profile: {}", profile.display_name))
            .await?
        {
            let my_id = self.identity.read().await.master_pubkey.clone();
            self.attempt_push(PushMessage {
                author: my_id,
                posts: vec![],
                interactions: vec![],
                profile: Some(profile.clone()),
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_post(&self, post: &Post) -> Result<(), AppError> {
        let msg = GossipMessage::NewPost(post.clone());
        if !self
            .broadcast_msg(&msg, &format!("post {}", &post.id))
            .await?
        {
            let my_id = self.identity.read().await.master_pubkey.clone();
            self.attempt_push(PushMessage {
                author: my_id,
                posts: vec![post.clone()],
                interactions: vec![],
                profile: None,
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
    ) -> Result<(), AppError> {
        let msg = GossipMessage::DeletePost {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        if !self.broadcast_msg(&msg, &format!("delete {id}")).await? {
            log::debug!("[gossip] delete post {id}: no gossip sender, peers will sync");
        }
        Ok(())
    }

    pub async fn broadcast_interaction(&self, interaction: &Interaction) -> Result<(), AppError> {
        let msg = GossipMessage::NewInteraction(interaction.clone());
        let label = format!(
            "{:?} on post {}",
            interaction.kind, &interaction.target_post_id
        );
        if !self.broadcast_msg(&msg, &label).await? {
            let my_id = self.identity.read().await.master_pubkey.clone();
            self.attempt_push(PushMessage {
                author: my_id,
                posts: vec![],
                interactions: vec![interaction.clone()],
                profile: None,
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
    ) -> Result<(), AppError> {
        let msg = GossipMessage::DeleteInteraction {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        if !self
            .broadcast_msg(&msg, &format!("delete interaction {id}"))
            .await?
        {
            log::debug!("[gossip] delete interaction {id}: no gossip sender, peers will sync");
        }
        Ok(())
    }

    pub async fn broadcast_linked_devices(
        &self,
        announcement: &LinkedDevicesAnnouncement,
    ) -> Result<(), AppError> {
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
        rotation: &SigningKeyRotation,
    ) -> Result<(), AppError> {
        let msg = GossipMessage::SigningKeyRotation(rotation.clone());
        self.broadcast_msg(
            &msg,
            &format!("signing key rotation to index {}", rotation.new_key_index),
        )
        .await?;
        Ok(())
    }

    /// Announce a new Stage room on the host's own user-feed gossip topic so followers discover it.
    pub async fn broadcast_stage_announcement(
        &self,
        stage_id: String,
        title: String,
        ticket: StageTicket,
        host_pubkey: String,
        started_at: u64,
    ) -> Result<(), AppError> {
        let msg = GossipMessage::StageAnnouncement {
            stage_id,
            title,
            ticket,
            host_pubkey,
            started_at,
        };
        self.broadcast_msg(&msg, "stage announcement").await?;
        Ok(())
    }

    /// Broadcast that a Stage room has ended on the host's own user-feed gossip topic.
    pub async fn broadcast_stage_ended(&self, stage_id: String) -> Result<(), AppError> {
        let msg = GossipMessage::StageEnded { stage_id };
        self.broadcast_msg(&msg, "stage ended").await?;
        Ok(())
    }

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
                Self::handle_new_post(storage, pk, my_id, app_handle, post).await;
            }
            Ok(GossipMessage::DeletePost {
                id,
                author,
                signature,
            }) => {
                Self::handle_delete_post(storage, pk, app_handle, &id, &author, &signature).await;
            }
            Ok(GossipMessage::ProfileUpdate(profile)) => {
                Self::handle_profile_update(storage, pk, app_handle, profile).await;
            }
            Ok(GossipMessage::NewInteraction(interaction)) => {
                Self::handle_new_interaction(storage, pk, my_id, app_handle, interaction).await;
            }
            Ok(GossipMessage::DeleteInteraction {
                id,
                author,
                signature,
            }) => {
                Self::handle_delete_interaction(storage, pk, app_handle, &id, &author, &signature)
                    .await;
            }
            Ok(GossipMessage::LinkedDevices(announcement)) => {
                Self::handle_linked_devices(storage, pk, announcement).await;
            }
            Ok(GossipMessage::SigningKeyRotation(rotation)) => {
                Self::handle_signing_key_rotation(storage, pk, rotation).await;
            }
            Ok(GossipMessage::Heartbeat) => {}
            Ok(GossipMessage::StageAnnouncement {
                stage_id,
                title,
                ticket,
                host_pubkey,
                started_at,
            }) => {
                log::info!(
                    "[gossip-rx] stage announcement: {} ({})",
                    title,
                    short_id(&stage_id)
                );
                let _ = app_handle.emit(
                    "stage-announced",
                    serde_json::json!({
                        "stage_id": stage_id,
                        "title": title,
                        "ticket": ticket.to_string(),
                        "host_pubkey": host_pubkey,
                        "started_at": started_at,
                    }),
                );
            }
            Ok(GossipMessage::StageEnded { stage_id }) => {
                log::info!("[gossip-rx] stage ended: {}", short_id(&stage_id));
                let _ = app_handle.emit("stage-ended-remote", &stage_id);
            }
            Err(e) => {
                log::error!("[gossip-rx] failed to parse message: {e}");
            }
        }
    }

    async fn handle_new_post(
        storage: &Storage,
        pk: &str,
        my_id: &str,
        app_handle: &AppHandle,
        post: Post,
    ) {
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
        } else if process_incoming_post(storage, &post, "gossip-rx", my_id, app_handle).await {
            let _ = app_handle.emit("feed-updated", ());
        }
    }

    async fn handle_delete_post(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        id: &str,
        author: &str,
        signature: &str,
    ) {
        if author != pk {
            return;
        }
        if let Some(signer) = crate::ingest::resolve_signer(storage, pk).await
            && let Err(reason) = verify_delete_post_signature(id, author, signature, &signer)
        {
            log::warn!(
                "[gossip-rx] bad delete-post signature from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        match storage.get_post_by_id(id).await {
            Ok(Some(post)) if post.author == pk => {
                log::info!("[gossip-rx] delete post {id} from {}", short_id(pk));
                if let Err(e) = storage.delete_post(id).await {
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

    async fn handle_profile_update(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        profile: Profile,
    ) {
        if let Err(reason) = validate_profile(&profile) {
            log::error!(
                "[gossip-rx] rejected profile from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        if let Some(signer) = crate::ingest::resolve_signer(storage, pk).await
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

    async fn handle_new_interaction(
        storage: &Storage,
        pk: &str,
        my_id: &str,
        app_handle: &AppHandle,
        interaction: Interaction,
    ) {
        if interaction.author != pk {
            return;
        }
        if storage.is_hidden(pk).await.unwrap_or(false) {
            log::info!(
                "[gossip-rx] skipping interaction from muted/blocked {}",
                short_id(pk)
            );
            return;
        }
        process_incoming_interaction(storage, &interaction, pk, "gossip-rx", my_id, app_handle)
            .await;
        let _ = app_handle.emit("interaction-received", &interaction);
    }

    async fn handle_delete_interaction(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        id: &str,
        author: &str,
        signature: &str,
    ) {
        if author != pk {
            return;
        }
        if let Some(signer) = crate::ingest::resolve_signer(storage, pk).await
            && let Err(reason) = verify_delete_interaction_signature(id, author, signature, &signer)
        {
            log::warn!(
                "[gossip-rx] bad delete-interaction signature from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        log::info!("[gossip-rx] delete interaction {id} from {}", short_id(pk));
        if let Err(e) = storage.delete_interaction(id, author).await {
            log::error!("[gossip-rx] failed to delete interaction: {e}");
        }
        let _ = app_handle.emit(
            "interaction-deleted",
            serde_json::json!({ "id": id, "author": author }),
        );
    }

    async fn handle_linked_devices(
        storage: &Storage,
        pk: &str,
        announcement: LinkedDevicesAnnouncement,
    ) {
        if announcement.master_pubkey != pk {
            log::warn!(
                "[gossip-rx] ignoring device announcement from {} (expected {})",
                short_id(&announcement.master_pubkey),
                short_id(pk)
            );
            return;
        }
        if let Err(reason) = verify_linked_devices_announcement(&announcement) {
            log::warn!(
                "[gossip-rx] bad device announcement from {}: {reason}",
                short_id(pk)
            );
            return;
        }
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

    async fn handle_signing_key_rotation(
        storage: &Storage,
        pk: &str,
        rotation: SigningKeyRotation,
    ) {
        if rotation.master_pubkey != pk {
            log::warn!(
                "[gossip-rx] ignoring key rotation from {} (expected {})",
                short_id(&rotation.master_pubkey),
                short_id(pk)
            );
            return;
        }
        if let Err(reason) = verify_rotation(&rotation) {
            log::warn!(
                "[gossip-rx] bad key rotation from {}: {reason}",
                short_id(pk)
            );
            return;
        }
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
        let response = proscenium_types::IdentityResponse {
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
