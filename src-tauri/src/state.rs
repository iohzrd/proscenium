use crate::dm::DmHandler;
use crate::gossip::GossipHandle;
use crate::opengraph::OgCache;
use crate::storage::Storage;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use std::sync::Arc;
use tokio::sync::mpsc;
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
    /// Signing public key string.
    #[allow(dead_code)]
    pub signing_pubkey: String,
    /// Signing key derivation index (0 for initial, incremented on rotation).
    pub signing_key_index: u32,
    /// DM public key string (hex-encoded X25519).
    pub dm_pubkey: String,
    /// DM key derivation index.
    pub dm_key_index: u32,
    /// Transport NodeId string (iroh's own key, for QUIC networking).
    pub transport_node_id: String,
    /// The current signing key delegation (signed by master key).
    pub delegation: iroh_social_types::SigningKeyDelegation,
}

pub struct AppState {
    pub identity: Identity,
    pub endpoint: Endpoint,
    /// Kept alive to maintain protocol handler registrations (DM, blobs, etc.)
    #[allow(dead_code)]
    pub router: Router,
    pub blobs: BlobsProtocol,
    pub store: FsStore,
    pub storage: Arc<Storage>,
    pub gossip: GossipHandle,
    pub dm: DmHandler,
    /// Active device-linking session (if any).
    pub pending_link: PendingLinkState,
    /// Shared HTTP client for server API calls and link preview fetching.
    pub http_client: reqwest::Client,
    /// Cache for OpenGraph link previews (TTL-based eviction).
    pub og_cache: OgCache,
    /// Send commands to the sync task (trigger on-demand syncs).
    #[allow(dead_code)]
    pub sync_tx: mpsc::Sender<SyncCommand>,
    /// Cancellation token for graceful shutdown of background tasks.
    /// Held here so child tokens stay alive; cancel via `shutdown_token.cancel()`.
    pub shutdown_token: CancellationToken,
}
