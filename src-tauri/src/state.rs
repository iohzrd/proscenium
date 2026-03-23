use crate::call::CallHandler;
use crate::dm::DmHandler;
use crate::gossip::GossipService;
use crate::peer::PeerHandler;
use crate::stage::StageHandler;
use crate::storage::Storage;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use std::sync::Arc;
use tokio::sync::mpsc;
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

/// Shared, mutable identity -- the canonical source for all key material.
pub type SharedIdentity = Arc<tokio::sync::RwLock<Identity>>;

/// Networking and protocol state that gets rebuilt during identity recovery.
pub struct NetworkStack {
    pub(crate) endpoint: Endpoint,
    pub(crate) gossip: GossipService,
    pub(crate) dm: DmHandler,
    pub(crate) call: CallHandler,
    pub(crate) peer: PeerHandler,
    pub(crate) stage: StageHandler,
    pub(crate) blobs: BlobsProtocol,
    pub(crate) sync_tx: mpsc::Sender<SyncCommand>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) _router: Router,
}

pub struct AppState {
    pub(crate) app_handle: tauri::AppHandle,
    pub(crate) identity: SharedIdentity,
    pub(crate) storage: Arc<Storage>,
    pub(crate) blob_store: FsStore,
    pub(crate) http_client: reqwest::Client,
    pub(crate) net: std::sync::RwLock<NetworkStack>,
}

impl AppState {
    pub fn endpoint(&self) -> Endpoint {
        self.net.read().unwrap().endpoint.clone()
    }

    pub fn gossip(&self) -> GossipService {
        self.net.read().unwrap().gossip.clone()
    }

    pub fn dm(&self) -> DmHandler {
        self.net.read().unwrap().dm.clone()
    }

    pub fn call(&self) -> CallHandler {
        self.net.read().unwrap().call.clone()
    }

    pub fn peer(&self) -> PeerHandler {
        self.net.read().unwrap().peer.clone()
    }

    pub fn stage(&self) -> StageHandler {
        self.net.read().unwrap().stage.clone()
    }

    pub fn blobs(&self) -> BlobsProtocol {
        self.net.read().unwrap().blobs.clone()
    }

    pub fn sync_tx(&self) -> mpsc::Sender<SyncCommand> {
        self.net.read().unwrap().sync_tx.clone()
    }

    pub fn shutdown(&self) -> CancellationToken {
        self.net.read().unwrap().shutdown.clone()
    }

    pub fn replace_net(&self, net: NetworkStack) {
        *self.net.write().unwrap() = net;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app_handle: tauri::AppHandle,
        identity: SharedIdentity,
        storage: Arc<Storage>,
        blob_store: FsStore,
        http_client: reqwest::Client,
        net: NetworkStack,
    ) -> Self {
        Self {
            app_handle,
            identity,
            storage,
            blob_store,
            http_client,
            net: std::sync::RwLock::new(net),
        }
    }
}
