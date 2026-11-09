mod broadcast;
mod handlers;
mod health;
mod own_feed;
mod subscriptions;

#[cfg(target_os = "android")]
use crate::constants::ANDROID_NET_INTERVAL;
use crate::constants::{MAX_NODE_IDS_TRIED, PUSH_CONCURRENCY};
use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::Endpoint;
use iroh_gossip::{Gossip, api::GossipSender};
use proscenium_types::{Profile, PushMessage, Visibility, short_id};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

pub(crate) struct GossipInner {
    pub(crate) my_sender: Option<GossipSender>,
    pub(crate) own_feed_handle: Option<JoinHandle<()>>,
    /// Maps a followed user's master pubkey to its subscription task.
    pub(crate) subscriptions: HashMap<String, JoinHandle<()>>,
    /// Receiver for reconnect requests; taken by `start_background`.
    pub(crate) reconnect_rx: Option<mpsc::UnboundedReceiver<String>>,
}

/// Cloneable handle to the gossip state. Its methods run on the caller's task. The one
/// internal channel carries reconnect requests from subscription tasks to the reconnect loop.
#[derive(Clone)]
pub struct GossipService {
    pub(crate) gossip: Gossip,
    pub(crate) endpoint: Endpoint,
    pub(crate) identity: SharedIdentity,
    pub(crate) storage: Arc<Storage>,
    pub(crate) app_handle: AppHandle,
    pub(crate) inner: Arc<tokio::sync::Mutex<GossipInner>>,
    /// Sender for reconnect requests; cloned into each subscription task.
    pub(crate) reconnect_tx: mpsc::UnboundedSender<String>,
    /// Limits the push connections open at the same time, over all pending pushes.
    push_limit: Arc<Semaphore>,
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
            push_limit: Arc::new(Semaphore::new(PUSH_CONCURRENCY)),
        }
    }

    /// Return a clone of the underlying iroh-gossip handle for direct topic subscriptions
    /// (e.g., Stage control plane topics that are separate from user feed topics).
    pub fn gossip_handle(&self) -> Gossip {
        self.gossip.clone()
    }

    /// Subscribe to all current follows, then spawn the reconnect loop and the
    /// network-health task. A second call does nothing.
    pub async fn start_background(&self, token: CancellationToken) {
        let Some(rx) = self.inner.lock().await.reconnect_rx.take() else {
            log::warn!("[gossip] start_background called more than once");
            return;
        };

        let my_id = self.identity.read().await.master_pubkey.clone();
        let follows = match self.storage.get_follows(&my_id).await {
            Ok(follows) => follows,
            Err(e) => {
                log::error!("[gossip] failed to load follows: {e}");
                Vec::new()
            }
        };
        for f in follows {
            let node_ids = self
                .storage
                .get_peer_transport_node_ids(&f.pubkey)
                .await
                .unwrap_or_default();
            let subscribed = if node_ids.is_empty() {
                log::warn!(
                    "[gossip] no NodeIds for {}, subscribe is retried later",
                    short_id(&f.pubkey)
                );
                false
            } else {
                match self.follow_user(f.pubkey.clone(), node_ids).await {
                    Ok(()) => true,
                    Err(e) => {
                        log::error!(
                            "[gossip] failed to subscribe to {} at startup: {e}",
                            short_id(&f.pubkey)
                        );
                        false
                    }
                }
            };
            if !subscribed {
                let _ = self.reconnect_tx.send(f.pubkey);
            }
        }

        let this = self.clone();
        let reconnect_token = token.child_token();
        tokio::spawn(async move {
            this.reconnect_loop(rx, reconnect_token).await;
        });

        // Network-health task: detects a suspend and resume of the process.
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

    /// The local user's visibility. An error means it could not be read; callers must
    /// not treat that as `Public`.
    pub(crate) async fn my_visibility(&self) -> Result<Visibility, AppError> {
        let my_id = self.identity.read().await.master_pubkey.clone();
        self.storage.get_visibility(&my_id).await
    }

    /// Send `msg` directly to every push recipient (see `Storage::get_push_recipients`).
    ///
    /// Returns after spawning the delivery task. If `msg.announcement` is empty it is set
    /// to the local user's signed device list, so recipients hold the current NodeIds and
    /// DM pubkey. At most `PUSH_CONCURRENCY` connections are open at
    /// once over all pending pushes, and at most `MAX_NODE_IDS_TRIED` NodeIds are tried
    /// per recipient. A recipient that does not acknowledge receives the content at its next sync.
    pub(crate) async fn attempt_push(&self, mut msg: PushMessage) {
        let my_id = self.identity.read().await.master_pubkey.clone();
        let recipients = match self.my_visibility().await {
            Ok(visibility) => self.storage.get_push_recipients(&my_id, visibility).await,
            Err(e) => Err(e),
        };
        let recipients = match recipients {
            Ok(recipients) => recipients,
            Err(e) => {
                log::error!("[push] failed to determine recipients: {e}");
                return;
            }
        };
        if recipients.is_empty() {
            return;
        }
        if msg.announcement.is_none() {
            match crate::announcement::build_own_announcement(&self.storage, &self.identity).await {
                Ok(announcement) => msg.announcement = Some(announcement),
                Err(e) => log::error!("[push] failed to build device announcement: {e}"),
            }
        }

        let endpoint = self.endpoint.clone();
        let storage = self.storage.clone();
        let limit = self.push_limit.clone();
        let msg = Arc::new(msg);
        tokio::spawn(async move {
            let mut deliveries = JoinSet::new();
            for recipient in recipients {
                // One permit per recipient, taken before the task is spawned.
                let Ok(permit) = limit.clone().acquire_owned().await else {
                    break;
                };
                let endpoint = endpoint.clone();
                let storage = storage.clone();
                let msg = msg.clone();
                deliveries.spawn(async move {
                    let _permit = permit;
                    push_to_recipient(&endpoint, &storage, &recipient, &msg).await;
                });
                while deliveries.try_join_next().is_some() {}
            }
            while deliveries.join_next().await.is_some() {}
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
}

/// Deliver `msg` to the first of the recipient's cached NodeIds that acknowledges it.
async fn push_to_recipient(
    endpoint: &Endpoint,
    storage: &Storage,
    recipient: &str,
    msg: &PushMessage,
) {
    let node_ids = storage
        .get_peer_transport_node_ids(recipient)
        .await
        .unwrap_or_default();
    for node_id in node_ids.iter().take(MAX_NODE_IDS_TRIED) {
        let Ok(target) = node_id.parse() else {
            continue;
        };
        if crate::push::push_to_peer(endpoint, target, msg)
            .await
            .is_ok()
        {
            return;
        }
    }
    log::debug!(
        "[push] {} did not acknowledge, it receives the content at its next sync",
        short_id(recipient)
    );
}
