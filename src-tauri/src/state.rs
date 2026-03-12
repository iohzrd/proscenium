use crate::dm::DmHandler;
use crate::gossip::FeedManager;
use crate::storage::Storage;
use iroh::{Endpoint, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

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

pub struct AppState {
    pub endpoint: Endpoint,
    /// Kept alive to maintain protocol handler registrations (DM, blobs, etc.)
    #[allow(dead_code)]
    pub router: Router,
    pub blobs: BlobsProtocol,
    pub store: FsStore,
    pub storage: Arc<Storage>,
    pub feed: Arc<RwLock<FeedManager>>,
    pub dm: DmHandler,
    /// Master key secret bytes (permanent identity, cold storage).
    pub master_secret_key_bytes: [u8; 32],
    /// Master public key string (the permanent, unforgeable identity).
    pub master_pubkey: String,
    /// Signing key secret bytes (derived from master, signs content).
    pub signing_secret_key_bytes: [u8; 32],
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
    /// Active device-linking session (if any).
    pub pending_link: PendingLinkState,
    /// Shared HTTP client for server API calls and link preview fetching.
    pub http_client: reqwest::Client,
    /// Cancellation token for graceful shutdown of background tasks.
    /// Held here so child tokens stay alive; cancel via `shutdown_token.cancel()`.
    #[allow(dead_code)]
    pub shutdown_token: CancellationToken,
}

pub fn generate_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("failed to generate random bytes");
    let (a, b) = bytes.split_at(8);
    format!(
        "{:016x}{:016x}",
        u64::from_le_bytes(a.try_into().unwrap()),
        u64::from_le_bytes(b.try_into().unwrap())
    )
}
