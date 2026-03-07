use crate::node::Node;
use crate::storage::Storage;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::Gossip;
use iroh_social_types::{
    GossipMessage, Interaction, PEER_ALPN, PeerRequest, Post, SyncRequest, short_id,
    user_feed_topic, validate_interaction, validate_post, validate_profile,
    verify_interaction_signature, verify_post_signature,
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
        let mut subs = self.subscriptions.lock().await;
        if subs.contains_key(pubkey) {
            return Ok(());
        }

        let topic = user_feed_topic(pubkey);
        let bootstrap: EndpointId = pubkey.parse()?;
        let topic_handle = self.gossip.subscribe(topic, vec![bootstrap]).await?;
        let (_sender, receiver) = topic_handle.split();

        let storage = self.storage.clone();
        let pk = pubkey.to_string();
        let handle = tokio::spawn(async move {
            tracing::info!("[ingestion] listening on gossip for {}", short_id(&pk));
            let mut receiver = receiver;
            loop {
                match receiver.try_next().await {
                    Ok(Some(event)) => {
                        if let iroh_gossip::api::Event::Received(msg) = &event {
                            Self::process_gossip_message(&storage, &pk, &msg.content).await;
                        }
                    }
                    Ok(None) => {
                        tracing::info!("[ingestion] gossip stream ended for {}", short_id(&pk));
                        break;
                    }
                    Err(e) => {
                        tracing::error!("[ingestion] gossip error for {}: {e}", short_id(&pk));
                        break;
                    }
                }
            }
        });

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

    async fn process_gossip_message(storage: &Storage, topic_owner: &str, data: &Bytes) {
        match serde_json::from_slice::<GossipMessage>(data) {
            Ok(GossipMessage::NewPost(post)) => {
                if post.author != topic_owner {
                    return;
                }
                Self::ingest_post(storage, &post).await;
            }
            Ok(GossipMessage::DeletePost { id, author }) => {
                if author != topic_owner {
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
            Ok(GossipMessage::DeleteInteraction { id, author }) => {
                if author != topic_owner {
                    return;
                }
                let _ = storage.delete_interaction(&id, &author).await;
                tracing::info!(
                    "[ingestion] deleted interaction {id} from {}",
                    short_id(&author)
                );
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
        if let Err(reason) = verify_post_signature(post) {
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
        if let Err(reason) = verify_interaction_signature(interaction) {
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
        let target: EndpointId = pubkey.parse()?;

        let last_post_ts = storage.get_last_post_timestamp(pubkey).await?;
        let last_interaction_ts = storage.get_last_interaction_timestamp(pubkey).await?;

        let addr = EndpointAddr::from(target);
        let conn = endpoint.connect(addr, PEER_ALPN).await?;

        let (mut send, mut recv) = conn.open_bi().await?;
        let request = PeerRequest::Sync(SyncRequest {
            author: pubkey.to_string(),
            post_count: 0,
            interaction_count: 0,
            newest_timestamp: last_post_ts.unwrap_or(0),
            newest_interaction_timestamp: last_interaction_ts.unwrap_or(0),
        });
        let request_bytes = serde_json::to_vec(&request)?;
        send.write_all(&(request_bytes.len() as u32).to_be_bytes())
            .await?;
        send.write_all(&request_bytes).await?;

        // Read SyncSummary
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        recv.read_exact(&mut buf).await?;
        let summary: iroh_social_types::SyncSummary = serde_json::from_slice(&buf)?;

        if let Some(profile) = &summary.profile
            && validate_profile(profile).is_ok()
        {
            let _ = storage.update_profile(pubkey, profile).await;
        }

        if summary.mode == iroh_social_types::SyncMode::UpToDate {
            storage.update_sync_state(pubkey, None, None).await?;
            return Ok((0, 0));
        }

        let (_send2, mut recv2) = conn.open_bi().await?;

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
