use crate::dm::DmHandler;
use crate::gossip::GossipService;
use crate::opengraph::OgCache;
use crate::storage::Storage;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::{Id, JoinSet};
use tokio_util::sync::CancellationToken;

/// Commands that can be sent to the sync task to trigger on-demand syncs.
#[allow(dead_code)]
pub enum SyncCommand {
    /// Sync a specific peer immediately.
    SyncPeer(String),
    /// Sync all followed peers immediately.
    SyncAll,
}

/// Active device-linking session on the existing device.
/// Created when the user taps "Link New Device", consumed when a new device connects.
#[derive(Debug)]
pub struct PendingLink {
    /// The one-time PSK (32 bytes) for Noise IK+PSK handshake.
    pub psk: [u8; 32],
    /// The existing device's X25519 private key (for Noise handshake).
    pub x25519_private: [u8; 32],
    /// When this pending link expires (Unix timestamp ms).
    pub expires_at: u64,
    /// Whether to include the master secret key in the bundle.
    pub transfer_master_key: bool,
}

pub type PendingLinkState = Arc<tokio::sync::Mutex<Option<PendingLink>>>;

/// Cryptographic identity and key material for this node.
pub struct Identity {
    /// Master key secret bytes (permanent identity, cold storage).
    pub master_secret_key_bytes: [u8; 32],
    /// Master public key string (the permanent, unforgeable identity).
    pub master_pubkey: String,
    /// Signing key secret bytes (derived from master, signs content).
    pub signing_secret_key_bytes: [u8; 32],
    /// Pre-constructed signing SecretKey (avoids repeated from_bytes).
    pub signing_key: SecretKey,
    /// Signing key derivation index (0 for initial, incremented on rotation).
    pub signing_key_index: u32,
    /// DM key secret bytes (derived from master, used for Noise IK + Double Ratchet).
    pub dm_secret_key_bytes: [u8; 32],
    /// DM public key string (hex-encoded X25519).
    pub dm_pubkey: String,
    /// DM key derivation index.
    pub dm_key_index: u32,
    /// Transport NodeId string (iroh's own key, for QUIC networking).
    pub transport_node_id: String,
    /// The current signing key delegation (signed by master key).
    pub delegation: iroh_social_types::SigningKeyDelegation,
}

/// All network-layer services: QUIC endpoint, protocol handles, and the router
/// that keeps protocol handlers registered and alive.
pub struct Net {
    pub endpoint: Endpoint,
    pub gossip: GossipService,
    pub dm: DmHandler,
    pub blobs: BlobsProtocol,
    // Held alive to keep all protocol handler registrations active.
    // Not used directly after construction.
    _router: Router,
}

impl Net {
    pub fn new(
        endpoint: Endpoint,
        gossip: GossipService,
        dm: DmHandler,
        blobs: BlobsProtocol,
        router: Router,
    ) -> Self {
        Self {
            endpoint,
            gossip,
            dm,
            blobs,
            _router: router,
        }
    }
}

/// Tracks all background tasks by name for structured shutdown.
pub struct TaskManager {
    tasks: JoinSet<()>,
    names: HashMap<Id, &'static str>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: JoinSet::new(),
            names: HashMap::new(),
        }
    }

    /// Spawn a named background task and track it.
    pub fn spawn(
        &mut self,
        name: &'static str,
        fut: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let handle = self.tasks.spawn(fut);
        self.names.insert(handle.id(), name);
    }

    /// Drain all tasks with a timeout. Returns names of tasks that were force-killed.
    pub async fn shutdown(mut self, timeout: std::time::Duration) -> Vec<&'static str> {
        let total = self.tasks.len();
        let mut completed = 0;
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, self.tasks.join_next_with_id()).await {
                Ok(Some(Ok((id, ())))) => {
                    completed += 1;
                    let name = self.names.get(&id).copied().unwrap_or("unknown");
                    log::info!(
                        "[shutdown] task '{}' completed ({}/{})",
                        name,
                        completed,
                        total
                    );
                }
                Ok(Some(Err(e))) => {
                    completed += 1;
                    let name = self.names.get(&e.id()).copied().unwrap_or("unknown");
                    log::error!("[shutdown] task '{}' panicked: {}", name, e);
                }
                Ok(None) => {
                    log::info!("[shutdown] all {} tasks completed cleanly", total);
                    return Vec::new();
                }
                Err(_) => break,
            }
        }

        // Force-abort remaining tasks
        let mut timed_out = Vec::new();
        self.tasks.abort_all();
        while let Some(result) = self.tasks.join_next_with_id().await {
            match result {
                Ok((id, ())) => {
                    let name = self.names.get(&id).copied().unwrap_or("unknown");
                    log::info!("[shutdown] task '{}' finished during abort", name);
                }
                Err(e) => {
                    let name = self.names.get(&e.id()).copied().unwrap_or("unknown");
                    if e.is_cancelled() {
                        log::warn!("[shutdown] task '{}' force-killed after timeout", name);
                        timed_out.push(name);
                    }
                }
            }
        }

        timed_out
    }
}

pub struct AppState {
    pub identity: Arc<Identity>,
    pub net: Net,
    pub storage: Arc<Storage>,
    /// Local blob storage (add/get bytes). For fetching remote blobs use net.blobs.
    pub blob_store: FsStore,
    pub http_client: reqwest::Client,
    pub og_cache: OgCache,
    /// Active device-linking session (if any).
    pub pending_link: PendingLinkState,
    /// Send commands to the sync task (trigger on-demand syncs).
    #[allow(dead_code)]
    pub sync_tx: mpsc::Sender<SyncCommand>,
    /// Cancellation token for graceful shutdown of background tasks.
    pub shutdown_token: CancellationToken,
    /// Tracked background tasks for structured shutdown.
    pub task_manager: tokio::sync::Mutex<Option<TaskManager>>,
}
