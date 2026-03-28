mod broadcast;
mod handlers;
mod health;
mod own_feed;
mod subscriptions;

#[cfg(target_os = "android")]
use crate::constants::ANDROID_NET_INTERVAL;
use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::Endpoint;
use iroh_gossip::{Gossip, api::GossipSender};
use proscenium_types::{Profile, PushMessage, Visibility, short_id};
use std::collections::HashMap;
use std::sync::{Arc, atomic::AtomicBool};
use tauri::AppHandle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(crate) struct GossipInner {
    pub(crate) my_sender: Option<GossipSender>,
    pub(crate) own_feed_handle: Option<JoinHandle<()>>,
    /// Maps peer master pubkey to (task_handle, has_neighbor_flag).
    pub(crate) subscriptions: HashMap<String, (JoinHandle<()>, Arc<AtomicBool>)>,
    /// Receiver for reconnect signals -- moved out by start_background.
    pub(crate) reconnect_rx: Option<mpsc::UnboundedReceiver<String>>,
}

/// Cloneable gossip service -- direct async methods, no command channel.
#[derive(Clone)]
pub struct GossipService {
    pub(crate) gossip: Gossip,
    pub(crate) endpoint: Endpoint,
    pub(crate) identity: SharedIdentity,
    pub(crate) storage: Arc<Storage>,
    pub(crate) app_handle: AppHandle,
    pub(crate) inner: Arc<tokio::sync::Mutex<GossipInner>>,
    /// Sender for reconnect signals -- cloned into each subscription task.
    pub(crate) reconnect_tx: mpsc::UnboundedSender<String>,
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

    pub(crate) async fn my_visibility(&self) -> Visibility {
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
    /// If a recipient is offline the message is dropped -- sync is the reliable fallback.
    pub(crate) async fn attempt_push(&self, msg: PushMessage) {
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
}
