use crate::dm::DmHandler;
use crate::gossip::FeedManager;
use crate::storage::Storage;
use iroh::{Endpoint, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct AppState {
    pub endpoint: Endpoint,
    /// Kept alive to maintain protocol handler registrations (DM, blobs, etc.)
    #[allow(dead_code)]
    pub router: Router,
    pub blobs: BlobsProtocol,
    pub store: FsStore,
    pub storage: Arc<Storage>,
    pub feed: Arc<Mutex<FeedManager>>,
    pub dm: DmHandler,
    /// Master key secret bytes (permanent identity, cold storage).
    #[allow(dead_code)]
    pub master_secret_key_bytes: [u8; 32],
    /// Master public key string (the permanent, unforgeable identity).
    pub master_pubkey: String,
    /// Signing key secret bytes (derived from master, signs content and DMs).
    pub signing_secret_key_bytes: [u8; 32],
    /// Signing public key string (will be used for delegation cache lookups).
    #[allow(dead_code)]
    pub signing_pubkey: String,
    /// Signing key derivation index (0 for initial, incremented on rotation).
    #[allow(dead_code)]
    pub signing_key_index: u32,
    /// Transport NodeId string (iroh's own key, for QUIC networking).
    pub transport_node_id: String,
    /// The current signing key delegation (signed by master key).
    pub delegation: iroh_social_types::SigningKeyDelegation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontendSyncResult {
    pub posts: Vec<iroh_social_types::Post>,
    pub remote_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub local_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    pub node_id: String,
    pub has_relay: bool,
    pub relay_url: Option<String>,
    pub follow_count: usize,
    pub follower_count: usize,
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
