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
use iroh::{Endpoint, EndpointId};
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
    /// Current neighbors on the feed topic of each followed user, by master pubkey. Each
    /// subscription task writes its entry on every neighbor change and removes it when
    /// it ends; `unfollow_user` removes the entry of the task it aborts.
    pub(crate) feed_neighbors: FeedNeighbors,
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
            feed_neighbors: FeedNeighbors::default(),
            inner: Arc::new(tokio::sync::Mutex::new(GossipInner {
                my_sender: None,
                own_feed_handle: None,
                subscriptions: HashMap::new(),
                reconnect_rx: Some(reconnect_rx),
            })),
            push_limit: Arc::new(Semaphore::new(PUSH_CONCURRENCY)),
        }
    }

    /// NodeIds that are currently neighbors on the feed topic of `pubkey`: devices
    /// subscribed to that topic, which are devices of `pubkey`, of its followers, or of a
    /// discovery server. Empty if the local user does not follow `pubkey`.
    pub fn feed_neighbors(&self, pubkey: &str) -> Vec<EndpointId> {
        self.feed_neighbors.get(pubkey)
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
    /// Returns after spawning the delivery task. An empty `msg.announcement` or
    /// `msg.profile` is set to the local user's signed device list or profile, so
    /// recipients hold the current NodeIds and visibility: a follower that stores non-Public
    /// posts then also stores the non-Public profile, and does not serve the posts as a
    /// mirror. At most `PUSH_CONCURRENCY` connections are open at
    /// once over all pending pushes, and at most `MAX_NODE_IDS_TRIED` NodeIds are tried
    /// per recipient: all of them for a stage announcement or end, else until one
    /// acknowledges (`push_to_recipient`). A recipient that does not acknowledge receives
    /// the content at its next sync.
    pub(crate) async fn attempt_push(&self, msg: PushMessage) {
        let _ = self.start_push(msg).await;
    }

    /// `attempt_push`, returning the delivery task, or `None` if no task was started (no
    /// recipient, or the recipients could not be read). Aborting the task ends the
    /// deliveries not yet acknowledged.
    pub(crate) async fn start_push(&self, mut msg: PushMessage) -> Option<JoinHandle<()>> {
        let my_id = self.identity.read().await.master_pubkey.clone();
        let recipients = match self.my_visibility().await {
            Ok(visibility) => self.storage.get_push_recipients(&my_id, visibility).await,
            Err(e) => Err(e),
        };
        let recipients = match recipients {
            Ok(recipients) => recipients,
            Err(e) => {
                log::error!("[push] failed to determine recipients: {e}");
                return None;
            }
        };
        if recipients.is_empty() {
            return None;
        }
        if msg.announcement.is_none() || msg.profile.is_none() {
            match crate::announcement::build_own_identity_response(&self.storage, &self.identity)
                .await
            {
                Ok(own) => {
                    msg.announcement.get_or_insert(own.announcement);
                    if msg.profile.is_none() {
                        msg.profile = own.profile;
                    }
                }
                Err(e) => log::error!("[push] failed to build device list and profile: {e}"),
            }
        }

        let endpoint = self.endpoint.clone();
        let storage = self.storage.clone();
        let limit = self.push_limit.clone();
        let msg = Arc::new(msg);
        // Dropping `deliveries` when the task is aborted aborts its delivery tasks.
        Some(tokio::spawn(async move {
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
        }))
    }

    /// Broadcast `profile`, whose visibility is not Public, on the own feed topic, then
    /// close the topic. Must be called before `profile` is saved: `broadcast_msg` sends on
    /// the topic only while the stored visibility is Public.
    pub async fn leave_public(&self, profile: &Profile) -> Result<(), AppError> {
        log::info!("[visibility] leaving Public for {}", profile.visibility);
        self.broadcast_profile(profile).await?;
        self.stop_own_feed().await;
        Ok(())
    }
}

/// Neighbor NodeIds per feed topic, keyed by the master pubkey of the topic's owner.
///
/// A `std::sync::Mutex`: every access is one map operation without an await point.
#[derive(Clone, Default)]
pub(crate) struct FeedNeighbors(Arc<std::sync::Mutex<HashMap<String, Vec<EndpointId>>>>);

impl FeedNeighbors {
    /// A poisoned lock is used anyway: every writer replaces or removes a whole entry.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<EndpointId>>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn get(&self, pubkey: &str) -> Vec<EndpointId> {
        self.lock().get(pubkey).cloned().unwrap_or_default()
    }

    /// Store `neighbors` for `pubkey`; an empty list removes the entry.
    pub(crate) fn set(&self, pubkey: &str, neighbors: Vec<EndpointId>) {
        let mut map = self.lock();
        if neighbors.is_empty() {
            map.remove(pubkey);
        } else {
            map.insert(pubkey.to_string(), neighbors);
        }
    }

    pub(crate) fn remove(&self, pubkey: &str) {
        self.lock().remove(pubkey);
    }
}

/// Deliver `msg` to the first `MAX_NODE_IDS_TRIED` of the recipient's cached NodeIds, in
/// list order, one after another. A message with a stage announcement or a stage end goes
/// to each of them: it is live, and device sync does not copy it to the recipient's other
/// devices. Any other message stops at the first NodeId that acknowledges it.
async fn push_to_recipient(
    endpoint: &Endpoint,
    storage: &Storage,
    recipient: &str,
    msg: &PushMessage,
) {
    let to_every_device = msg.stage.is_some() || msg.stage_ended.is_some();
    let node_ids = storage
        .get_peer_transport_node_ids(recipient)
        .await
        .unwrap_or_default();
    let mut acknowledged = false;
    for node_id in node_ids.iter().take(MAX_NODE_IDS_TRIED) {
        let Ok(target) = node_id.parse() else {
            continue;
        };
        if crate::push::push_to_peer(endpoint, target, msg)
            .await
            .is_ok()
        {
            acknowledged = true;
            if !to_every_device {
                return;
            }
        }
    }
    if !acknowledged {
        log::debug!(
            "[push] {} did not acknowledge, it receives the content at its next sync",
            short_id(recipient)
        );
    }
}
