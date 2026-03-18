use crate::call::CallHandler;
use crate::dm::DmHandler;
use crate::gossip::GossipService;
use crate::peer::PeerHandler;
use crate::stage::StageHandler;
use crate::storage::Storage;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;

/// Commands sent to the peer sync task to trigger on-demand syncs.
pub enum SyncCommand {
    /// Sync a specific peer immediately (e.g. after following).
    SyncPeer(String),
    /// Sync all followed peers immediately (e.g. app foreground, manual refresh).
    SyncAll,
}

/// Cryptographic identity and key material for this node.
/// Wrapped in `SharedIdentity` (Arc<RwLock<Identity>>) so key material can be
/// updated in place when signing keys or DM keys are rotated at runtime.
#[derive(Debug)]
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
    /// X25519 private key bytes (derived from dm_secret_key_bytes).
    pub dm_x25519_private: [u8; 32],
    /// X25519 public key bytes (derived from dm_secret_key_bytes).
    pub dm_x25519_public: [u8; 32],
    /// Key for encrypting ratchet state at rest (derived from dm_secret_key_bytes).
    pub ratchet_storage_key: [u8; 32],
    /// Transport NodeId string (iroh's own key, for QUIC networking).
    pub transport_node_id: String,
    /// The current signing key delegation (signed by master key).
    pub delegation: proscenium_types::SigningKeyDelegation,
}

/// Shared, mutable identity — the canonical source for all key material.
pub type SharedIdentity = Arc<RwLock<Identity>>;

pub struct AppState {
    pub(crate) app_handle: tauri::AppHandle,
    pub(crate) identity: SharedIdentity,
    pub(crate) storage: Arc<Storage>,
    /// Local blob storage (add/get bytes). For fetching remote blobs use `blobs`.
    pub(crate) blob_store: FsStore,
    pub(crate) http_client: reqwest::Client,
    pub(crate) gossip: GossipService,
    pub(crate) dm: DmHandler,
    pub(crate) call: CallHandler,
    pub(crate) peer: PeerHandler,
    pub(crate) stage: StageHandler,
    pub(crate) endpoint: Endpoint,
    pub(crate) blobs: BlobsProtocol,
    /// Send on-demand sync commands to the peer sync task.
    pub(crate) sync_tx: mpsc::Sender<SyncCommand>,
    /// Cancellation token for graceful shutdown of all background tasks.
    pub(crate) shutdown: CancellationToken,
    // Held alive to keep all protocol handler registrations active.
    _router: Router,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app_handle: tauri::AppHandle,
        identity: SharedIdentity,
        storage: Arc<Storage>,
        blob_store: FsStore,
        http_client: reqwest::Client,
        gossip: GossipService,
        dm: DmHandler,
        call: CallHandler,
        peer: PeerHandler,
        stage: StageHandler,
        endpoint: Endpoint,
        blobs: BlobsProtocol,
        router: Router,
        sync_tx: mpsc::Sender<SyncCommand>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            app_handle,
            identity,
            storage,
            blob_store,
            http_client,
            gossip,
            dm,
            call,
            peer,
            stage,
            endpoint,
            blobs,
            sync_tx,
            shutdown,
            _router: router,
        }
    }
}
