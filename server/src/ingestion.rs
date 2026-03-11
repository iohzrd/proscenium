use crate::node::Node;
use crate::storage::Storage;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::PublicKey;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::Gossip;
use iroh_social_types::{
    GossipMessage, Interaction, PEER_ALPN, PeerRequest, Post, SyncRequest, short_id,
    user_feed_topic, validate_interaction, validate_post, validate_profile,
    verify_delete_interaction_signature, verify_delete_post_signature,
    verify_interaction_signature, verify_linked_devices_announcement, verify_post_signature,
    verify_profile_signature, verify_rotation,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub struct IngestionManager {
    gossip: Gossip,
    pub endpoint: Endpoint,
    storage: Arc<Storage>,
    subscriptions: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl IngestionManager {
    pub fn new(node: &Node, storage: Arc<Storage>) -> Arc<Self> {
        Arc::new(Self {
            gossip: node.gossip.clone(),
            endpoint: node.endpoint.clone(),
            storage,
            subscriptions: Mutex::new(HashMap::new()),
        })
    }

    pub async fn subscribe(self: &Arc<Self>, pubkey: &str) -> anyhow::Result<()> {
        // Check if already subscribed without holding lock during network calls
        {
            let subs = self.subscriptions.lock().await;
            if subs.contains_key(pubkey) {
                return Ok(());
            }
        }

        let topic = user_feed_topic(pubkey);

        // Resolve transport NodeId from storage (registration or delegation cache)
        let bootstrap: Vec<EndpointId> =
            if let Ok(Some(node_id)) = self.storage.get_transport_node_id(pubkey).await {
                match node_id.parse() {
                    Ok(eid) => vec![eid],
                    Err(_) => vec![],
                }
            } else {
                vec![]
            };

        if bootstrap.is_empty() {
            anyhow::bail!(
                "no transport NodeId known for {}, cannot subscribe to gossip",
                short_id(pubkey)
            );
        }

        // Subscribe outside the lock to avoid holding mutex across await
        let topic_handle = self.gossip.subscribe(topic, bootstrap.clone()).await?;
        let (_sender, receiver) = topic_handle.split();

        let gossip = self.gossip.clone();
        let storage = self.storage.clone();
        let pk = pubkey.to_string();
        let handle = tokio::spawn(async move {
            tracing::info!("[ingestion] listening on gossip for {}", short_id(&pk));
            let mut _sender_hold = _sender;
            let mut receiver = receiver;
            let mut backoff = 1u64;

            loop {
                // Process events until stream ends or errors
                loop {
                    match receiver.try_next().await {
                        Ok(Some(event)) => {
                            if let iroh_gossip::api::Event::Received(msg) = &event {
                                Self::process_gossip_message(&storage, &pk, &msg.content).await;
                            }
                        }
                        Ok(None) => {
                            tracing::warn!(
                                "[ingestion] gossip stream ended for {}, will reconnect",
                                short_id(&pk)
                            );
                            break;
                        }
                        Err(e) => {
                            tracing::error!(
                                "[ingestion] gossip error for {}: {e}, will reconnect",
                                short_id(&pk)
                            );
                            break;
                        }
                    }
                }

                // Reconnect with exponential backoff
                loop {
                    tracing::info!(
                        "[ingestion] reconnecting to {} in {backoff}s",
                        short_id(&pk)
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    match gossip.subscribe(topic, bootstrap.clone()).await {
                        Ok(new_handle) => {
                            let (new_sender, new_receiver) = new_handle.split();
                            _sender_hold = new_sender;
                            receiver = new_receiver;
                            backoff = 1;
                            tracing::info!(
                                "[ingestion] reconnected to gossip for {}",
                                short_id(&pk)
                            );
                            break;
                        }
                        Err(e) => {
                            tracing::error!(
                                "[ingestion] failed to resubscribe to {}: {e}",
                                short_id(&pk)
                            );
                            backoff = (backoff * 2).min(60);
                        }
                    }
                }
            }
        });

        // Re-acquire lock to insert the handle
        let mut subs = self.subscriptions.lock().await;
        subs.insert(pubkey.to_string(), handle);
        tracing::info!("[ingestion] subscribed to {}", short_id(pubkey));
        Ok(())
    }

    pub async fn unsubscribe(&self, pubkey: &str) {
        let mut subs = self.subscriptions.lock().await;
        if let Some(handle) = subs.remove(pubkey) {
            handle.abort();
            tracing::info!("[ingestion] unsubscribed from {}", short_id(pubkey));
        }
    }

    /// Resolve the signing public key for a peer from the delegation cache.
    /// Returns None if no delegation is cached (backward compat: skip verification).
    async fn resolve_signer(storage: &Storage, master_pubkey: &str) -> Option<PublicKey> {
        match storage.get_peer_signing_pubkey(master_pubkey).await {
            Ok(Some(signing_pubkey)) => match signing_pubkey.parse() {
                Ok(pk) => Some(pk),
                Err(e) => {
                    tracing::warn!(
                        "[ingestion] bad signing pubkey for {}: {e}",
                        short_id(master_pubkey)
                    );
                    None
                }
            },
            _ => None,
        }
    }

    async fn process_gossip_message(storage: &Storage, topic_owner: &str, data: &Bytes) {
        match serde_json::from_slice::<GossipMessage>(data) {
            Ok(GossipMessage::NewPost(post)) => {
                if post.author != topic_owner {
                    return;
                }
                Self::ingest_post(storage, &post).await;
            }
            Ok(GossipMessage::DeletePost {
                id,
                author,
                signature,
            }) => {
                if author != topic_owner {
                    return;
                }
                // Verify delete signature
                if let Some(signer) = Self::resolve_signer(storage, &author).await
                    && let Err(reason) =
                        verify_delete_post_signature(&id, &author, &signature, &signer)
                {
                    tracing::warn!(
                        "[ingestion] bad delete-post signature from {}: {reason}",
                        short_id(&author)
                    );
                    return;
                }
                if let Ok(Some(existing)) = storage.get_post(&author, &id).await
                    && existing.author == author
                {
                    let _ = storage.delete_post(&id, &author).await;
                    tracing::info!("[ingestion] deleted post {id} from {}", short_id(&author));
                }
            }
            Ok(GossipMessage::ProfileUpdate(profile)) => {
                if let Err(reason) = validate_profile(&profile) {
                    tracing::warn!(
                        "[ingestion] rejected profile from {}: {reason}",
                        short_id(topic_owner)
                    );
                    return;
                }
                // Verify profile signature
                if let Some(signer) = Self::resolve_signer(storage, topic_owner).await
                    && let Err(reason) = verify_profile_signature(&profile, &signer)
                {
                    tracing::warn!(
                        "[ingestion] bad profile signature from {}: {reason}",
                        short_id(topic_owner)
                    );
                    return;
                }
                if let Err(e) = storage.update_profile(topic_owner, &profile).await {
                    tracing::error!("[ingestion] failed to update profile: {e}");
                } else {
                    tracing::info!(
                        "[ingestion] updated profile for {}: {}",
                        short_id(topic_owner),
                        profile.display_name
                    );
                    if let Err(e) = storage
                        .handle_visibility_change(topic_owner, profile.visibility)
                        .await
                    {
                        tracing::error!("[ingestion] visibility change error: {e}");
                    }
                }
            }
            Ok(GossipMessage::NewInteraction(interaction)) => {
                if interaction.author != topic_owner {
                    return;
                }
                Self::ingest_interaction(storage, &interaction).await;
            }
            Ok(GossipMessage::DeleteInteraction {
                id,
                author,
                signature,
            }) => {
                if author != topic_owner {
                    return;
                }
                // Verify delete signature
                if let Some(signer) = Self::resolve_signer(storage, &author).await
                    && let Err(reason) =
                        verify_delete_interaction_signature(&id, &author, &signature, &signer)
                {
                    tracing::warn!(
                        "[ingestion] bad delete-interaction signature from {}: {reason}",
                        short_id(&author)
                    );
                    return;
                }
                let _ = storage.delete_interaction(&id, &author).await;
                tracing::info!(
                    "[ingestion] deleted interaction {id} from {}",
                    short_id(&author)
                );
            }
            Ok(GossipMessage::LinkedDevices(announcement)) => {
                if announcement.master_pubkey != topic_owner {
                    return;
                }
                if let Err(reason) = verify_linked_devices_announcement(&announcement) {
                    tracing::warn!(
                        "[ingestion] bad device announcement from {}: {reason}",
                        short_id(topic_owner)
                    );
                    return;
                }
                // Skip stale announcements
                let cached_version = storage
                    .get_peer_announcement_version(topic_owner)
                    .await
                    .unwrap_or(None);
                if cached_version.is_some_and(|v| announcement.version as i64 <= v) {
                    return;
                }
                let transport_ids: Vec<String> = announcement
                    .devices
                    .iter()
                    .map(|d| d.node_id.clone())
                    .collect();
                let announcement_json = serde_json::to_string(&announcement).unwrap_or_default();
                if let Err(e) = storage
                    .cache_peer_device_announcement(
                        topic_owner,
                        &announcement_json,
                        announcement.version as i64,
                        &transport_ids,
                    )
                    .await
                {
                    tracing::error!("[ingestion] failed to cache device announcement: {e}");
                } else {
                    tracing::info!(
                        "[ingestion] cached device announcement for {} v{} ({} devices)",
                        short_id(topic_owner),
                        announcement.version,
                        announcement.devices.len()
                    );
                }
            }
            Ok(GossipMessage::SigningKeyRotation(rotation)) => {
                if rotation.master_pubkey != topic_owner {
                    return;
                }
                if let Err(reason) = verify_rotation(&rotation) {
                    tracing::warn!(
                        "[ingestion] bad key rotation from {}: {reason}",
                        short_id(topic_owner)
                    );
                    return;
                }
                // Reject replay/downgrade: check cached delegation's key_index
                if let Ok(Some(cached_json)) = sqlx::query_scalar::<_, String>(
                    "SELECT delegation_json FROM peer_delegations WHERE master_pubkey = ?1",
                )
                .bind(topic_owner)
                .fetch_optional(&storage.pool)
                .await
                    && let Ok(cached) = serde_json::from_str::<
                        iroh_social_types::SigningKeyDelegation,
                    >(&cached_json)
                    && rotation.new_key_index <= cached.key_index
                {
                    tracing::warn!(
                        "[ingestion] stale key rotation for {} (index {} <= cached {})",
                        short_id(topic_owner),
                        rotation.new_key_index,
                        cached.key_index
                    );
                    return;
                }
                // Update cached delegation with the new signing key
                let delegation_json =
                    serde_json::to_string(&rotation.new_delegation).unwrap_or_default();
                if let Err(e) = storage
                    .cache_peer_delegation(
                        topic_owner,
                        &rotation.new_signing_pubkey,
                        &delegation_json,
                        None,
                    )
                    .await
                {
                    tracing::error!("[ingestion] failed to cache rotated delegation: {e}");
                } else {
                    tracing::info!(
                        "[ingestion] signing key rotation for {} to index {}",
                        short_id(topic_owner),
                        rotation.new_key_index
                    );
                }
                // Also update the registration's delegation_json
                if let Err(e) =
                    sqlx::query("UPDATE registrations SET delegation_json = ?2 WHERE pubkey = ?1")
                        .bind(topic_owner)
                        .bind(&delegation_json)
                        .execute(&storage.pool)
                        .await
                {
                    tracing::error!("[ingestion] failed to update registration delegation: {e}");
                }
            }
            Ok(GossipMessage::Heartbeat) => {
                // Keep-alive ping from publisher, nothing to store.
            }
            Err(e) => {
                tracing::warn!("[ingestion] failed to parse gossip message: {e}");
            }
        }
    }

    async fn ingest_post(storage: &Storage, post: &Post) {
        if let Err(reason) = validate_post(post) {
            tracing::warn!("[ingestion] rejected post {}: {reason}", &post.id);
            return;
        }
        // Look up the signing key from the delegation cache
        let signer: PublicKey = match storage.get_peer_signing_pubkey(&post.author).await {
            Ok(Some(signing_pubkey)) => match signing_pubkey.parse() {
                Ok(pk) => pk,
                Err(e) => {
                    tracing::warn!(
                        "[ingestion] bad signing pubkey in delegation for {}: {e}",
                        short_id(&post.author)
                    );
                    return;
                }
            },
            _ => {
                // No delegation cached -- fall back to author as signer (backward compat)
                match post.author.parse() {
                    Ok(pk) => pk,
                    Err(e) => {
                        tracing::warn!("[ingestion] bad author pubkey on post {}: {e}", &post.id);
                        return;
                    }
                }
            }
        };
        if let Err(reason) = verify_post_signature(post, &signer) {
            tracing::warn!("[ingestion] bad signature on post {}: {reason}", &post.id);
            return;
        }
        match storage.insert_post(post).await {
            Ok(true) => {
                tracing::info!(
                    "[ingestion] stored post {} from {}",
                    &post.id,
                    short_id(&post.author)
                );
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!("[ingestion] failed to store post: {e}");
            }
        }
    }

    async fn ingest_interaction(storage: &Storage, interaction: &Interaction) {
        if let Err(reason) = validate_interaction(interaction) {
            tracing::warn!(
                "[ingestion] rejected interaction {}: {reason}",
                &interaction.id
            );
            return;
        }
        // Look up the signing key from the delegation cache
        let signer: PublicKey = match storage.get_peer_signing_pubkey(&interaction.author).await {
            Ok(Some(signing_pubkey)) => match signing_pubkey.parse() {
                Ok(pk) => pk,
                Err(e) => {
                    tracing::warn!(
                        "[ingestion] bad signing pubkey in delegation for {}: {e}",
                        short_id(&interaction.author)
                    );
                    return;
                }
            },
            _ => {
                // No delegation cached -- fall back to author as signer (backward compat)
                match interaction.author.parse() {
                    Ok(pk) => pk,
                    Err(e) => {
                        tracing::warn!(
                            "[ingestion] bad author pubkey on interaction {}: {e}",
                            &interaction.id
                        );
                        return;
                    }
                }
            }
        };
        if let Err(reason) = verify_interaction_signature(interaction, &signer) {
            tracing::warn!(
                "[ingestion] bad signature on interaction {}: {reason}",
                &interaction.id
            );
            return;
        }
        match storage.insert_interaction(interaction).await {
            Ok(true) => {
                tracing::info!(
                    "[ingestion] stored {:?} from {} on post {}",
                    interaction.kind,
                    short_id(&interaction.author),
                    &interaction.target_post_id
                );
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!("[ingestion] failed to store interaction: {e}");
            }
        }
    }

    pub async fn sync_from_peer(
        endpoint: &Endpoint,
        storage: &Storage,
        pubkey: &str,
    ) -> anyhow::Result<(usize, usize)> {
        // Wrap the entire sync operation in a timeout to prevent hung peers
        // from blocking the sync loop indefinitely
        const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        tokio::time::timeout(
            SYNC_TIMEOUT,
            Self::sync_from_peer_inner(endpoint, storage, pubkey),
        )
        .await
        .map_err(|_| anyhow::anyhow!("sync timed out after {}s", SYNC_TIMEOUT.as_secs()))?
    }

    async fn sync_from_peer_inner(
        endpoint: &Endpoint,
        storage: &Storage,
        pubkey: &str,
    ) -> anyhow::Result<(usize, usize)> {
        // Resolve transport NodeId from storage
        let node_id = storage
            .get_transport_node_id(pubkey)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no transport NodeId for {}", short_id(pubkey)))?;
        let target: EndpointId = node_id.parse()?;

        let last_post_ts = storage.get_last_post_timestamp(pubkey).await?;
        let last_interaction_ts = storage.get_last_interaction_timestamp(pubkey).await?;

        let addr = EndpointAddr::from(target);
        let conn = endpoint.connect(addr, PEER_ALPN).await?;

        // Phase 1: Send sync request, read summary
        let (mut send, mut recv) = conn.open_bi().await?;
        let request = PeerRequest::Sync(SyncRequest {
            author: pubkey.to_string(),
            post_count: 0,
            interaction_count: 0,
            newest_timestamp: last_post_ts.unwrap_or(0),
            newest_interaction_timestamp: last_interaction_ts.unwrap_or(0),
        });
        let request_bytes = serde_json::to_vec(&request)?;
        send.write_all(&request_bytes).await?;
        send.finish()?;

        let summary_bytes = recv.read_to_end(65_536).await?;
        let summary: iroh_social_types::SyncSummary = serde_json::from_slice(&summary_bytes)?;

        if let Some(profile) = &summary.profile
            && validate_profile(profile).is_ok()
        {
            let _ = storage.update_profile(pubkey, profile).await;
        }

        if summary.mode == iroh_social_types::SyncMode::UpToDate {
            storage.update_sync_state(pubkey, None, None).await?;
            return Ok((0, 0));
        }

        let (mut send2, mut recv2) = conn.open_bi().await?;
        // Must finish send side to trigger accept_bi() on the peer
        // (QUIC streams are lazy - peer won't see the stream until data/FIN is sent)
        send2.finish()?;

        let mut post_count = 0usize;
        let mut interaction_count = 0usize;
        let mut last_post = None;
        let mut last_int = None;

        loop {
            let mut len_buf = [0u8; 4];
            if recv2.read_exact(&mut len_buf).await.is_err() {
                break;
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len == 0 {
                break;
            }
            let mut buf = vec![0u8; len];
            if recv2.read_exact(&mut buf).await.is_err() {
                break;
            }

            match serde_json::from_slice::<iroh_social_types::SyncFrame>(&buf) {
                Ok(iroh_social_types::SyncFrame::Posts(posts)) => {
                    for post in &posts {
                        Self::ingest_post(storage, post).await;
                        post_count += 1;
                        let ts = post.timestamp as i64;
                        if last_post.is_none_or(|prev| ts > prev) {
                            last_post = Some(ts);
                        }
                    }
                }
                Ok(iroh_social_types::SyncFrame::Interactions(interactions)) => {
                    for interaction in &interactions {
                        Self::ingest_interaction(storage, interaction).await;
                        interaction_count += 1;
                        let ts = interaction.timestamp as i64;
                        if last_int.is_none_or(|prev| ts > prev) {
                            last_int = Some(ts);
                        }
                    }
                }
                Ok(iroh_social_types::SyncFrame::DeviceAnnouncements(announcements)) => {
                    for announcement in &announcements {
                        if announcement.master_pubkey != pubkey {
                            continue;
                        }
                        if verify_linked_devices_announcement(announcement).is_err() {
                            continue;
                        }
                        let transport_ids: Vec<String> = announcement
                            .devices
                            .iter()
                            .map(|d| d.node_id.clone())
                            .collect();
                        let json = serde_json::to_string(announcement).unwrap_or_default();
                        let _ = storage
                            .cache_peer_device_announcement(
                                pubkey,
                                &json,
                                announcement.version as i64,
                                &transport_ids,
                            )
                            .await;
                    }
                }
                Err(e) => {
                    tracing::warn!("[sync] failed to parse frame: {e}");
                    break;
                }
            }
        }

        storage
            .update_sync_state(pubkey, last_post, last_int)
            .await?;
        Ok((post_count, interaction_count))
    }

    pub async fn start(self: &Arc<Self>, startup_sync: bool) {
        let pubkeys = match self.storage.get_active_public_pubkeys().await {
            Ok(pks) => pks,
            Err(e) => {
                tracing::error!("[ingestion] failed to get public users: {e}");
                return;
            }
        };

        tracing::info!("[ingestion] subscribing to {} public users", pubkeys.len());

        for pk in &pubkeys {
            if let Err(e) = self.subscribe(pk).await {
                tracing::error!("[ingestion] failed to subscribe to {}: {e}", short_id(pk));
            }
        }

        if startup_sync {
            let mgr = self.clone();
            let pks = pubkeys;
            tokio::spawn(async move {
                tracing::info!("[sync] starting initial sync for {} users", pks.len());
                let semaphore = Arc::new(tokio::sync::Semaphore::new(10));
                let mut join_set = tokio::task::JoinSet::new();

                for pk in pks {
                    let ep = mgr.endpoint.clone();
                    let st = mgr.storage.clone();
                    let sem = semaphore.clone();
                    join_set.spawn(async move {
                        let _permit = sem.acquire().await;
                        match Self::sync_from_peer(&ep, &st, &pk).await {
                            Ok((posts, interactions)) => {
                                if posts > 0 || interactions > 0 {
                                    tracing::info!(
                                        "[sync] synced {posts} posts, {interactions} interactions from {}",
                                        short_id(&pk)
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "[sync] failed to sync from {}: {e}",
                                    short_id(&pk)
                                );
                            }
                        }
                    });
                }

                while let Some(result) = join_set.join_next().await {
                    if let Err(e) = result {
                        tracing::error!("[sync] task panicked: {e}");
                    }
                }
                tracing::info!("[sync] initial sync complete");
            });
        }
    }

    pub fn start_periodic_sync(self: &Arc<Self>, interval_minutes: u64) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(interval_minutes * 60);
            loop {
                tokio::time::sleep(interval).await;
                let pubkeys = match mgr.storage.get_active_public_pubkeys().await {
                    Ok(pks) => pks,
                    Err(e) => {
                        tracing::error!("[periodic-sync] failed to get users: {e}");
                        continue;
                    }
                };

                for pk in &pubkeys {
                    match Self::sync_from_peer(&mgr.endpoint, &mgr.storage, pk).await {
                        Ok((posts, interactions)) => {
                            if posts > 0 || interactions > 0 {
                                tracing::info!(
                                    "[periodic-sync] synced {posts} posts, {interactions} interactions from {}",
                                    short_id(pk)
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("[periodic-sync] failed to sync {}: {e}", short_id(pk));
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        });
    }
}
