use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    #[default]
    Public,
    Listed,
    Private,
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Visibility::Public => write!(f, "public"),
            Visibility::Listed => write!(f, "listed"),
            Visibility::Private => write!(f, "private"),
        }
    }
}

impl std::str::FromStr for Visibility {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "public" => Ok(Visibility::Public),
            "listed" => Ok(Visibility::Listed),
            "private" => Ok(Visibility::Private),
            other => Err(format!("unknown visibility: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub display_name: String,
    pub bio: String,
    pub avatar_hash: Option<String>,
    pub avatar_ticket: Option<String>,
    pub visibility: Visibility,
    #[serde(default)]
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAttachment {
    pub hash: String,
    pub ticket: String,
    pub mime_type: String,
    pub filename: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: u64,
    #[serde(default)]
    pub media: Vec<MediaAttachment>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub reply_to_author: Option<String>,
    pub quote_of: Option<String>,
    pub quote_of_author: Option<String>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    pub id: String,
    pub author: String,
    pub kind: InteractionKind,
    pub target_post_id: String,
    pub target_author: String,
    pub timestamp: u64,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InteractionKind {
    Like,
}

/// A device in a linked devices announcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// The device's iroh transport NodeId.
    pub node_id: String,
    /// Human-readable device name.
    pub device_name: String,
    /// Whether this is the primary device (created the signing key).
    pub is_primary: bool,
    /// When this device was added (Unix timestamp ms).
    pub added_at: u64,
}

/// A relationship in the social graph. Represents either a follow or follower
/// depending on query direction. All fields are present; unused ones default to 0/false.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialGraphEntry {
    pub pubkey: String,
    pub followed_at: u64,
    pub first_seen: u64,
    pub last_seen: u64,
    pub is_online: bool,
}

/// Encoded in the QR code displayed by the existing device during pairing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkQrPayload {
    /// Existing device's iroh NodeId (for QUIC connection).
    pub node_id: String,
    /// One-time secret for Noise PSK (32 bytes, base64-encoded).
    pub secret: String,
    /// Existing device's relay URL for connection (if available).
    pub relay_url: Option<String>,
}

/// Data bundle sent from existing device to new device during pairing.
/// Encrypted inside the Noise channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkBundleData {
    /// The signing key secret (32 bytes, base64-encoded).
    pub signing_secret_key: String,
    /// The DM key secret (32 bytes, base64-encoded).
    /// Used for X25519 DH (Noise IK + Double Ratchet).
    pub dm_secret_key: String,
    /// The signing key delegation (signed by master key).
    pub delegation: crate::delegation::SigningKeyDelegation,
    /// The new device's transport secret key (32 bytes, base64-encoded).
    /// Derived by the existing device from the master key.
    pub transport_secret_key: String,
    /// The device index used to derive the transport key.
    pub device_index: u32,
    /// The master secret key (32 bytes, base64-encoded).
    /// Only included if the sending device holds it AND user opts in.
    pub master_secret_key: Option<String>,
    /// User profile.
    pub profile: Option<Profile>,
    /// Follow list.
    pub follows: Vec<SocialGraphEntry>,
    /// Bookmarked post IDs.
    pub bookmarks: Vec<String>,
    /// Blocked user pubkeys.
    pub blocked_users: Vec<String>,
    /// Muted user pubkeys.
    pub muted_users: Vec<String>,
    /// Current DM ratchet sessions (serialized).
    pub ratchet_sessions: Vec<RatchetSessionExport>,
}

/// Exported ratchet session for transfer during device pairing and sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetSessionExport {
    pub peer_pubkey: String,
    pub state_json: String,
    #[serde(default)]
    pub updated_at: u64,
}

/// Follow entry with LWW state for device sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowEntry {
    pub pubkey: String,
    pub followed_at: u64,
    pub state: String,
    pub last_changed_at: u64,
}

/// Moderation entry (mute or block) with LWW state for device sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationEntry {
    pub pubkey: String,
    pub kind: String,
    pub created_at: u64,
    pub state: String,
    pub last_changed_at: u64,
}

/// Ratchet session summary for device sync vector comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetSyncEntry {
    pub peer_pubkey: String,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostCounts {
    pub likes: u32,
    pub replies: u32,
    pub reposts: u32,
    pub liked_by_me: bool,
    pub reposted_by_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub actor: String,
    pub target_post_id: Option<String>,
    pub post_id: Option<String>,
    pub timestamp: u64,
    pub read: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowRequestEntry {
    pub pubkey: String,
    pub timestamp: u64,
    pub status: String,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub url: String,
    pub name: String,
    pub description: String,
    pub node_id: String,
    pub registered_at: Option<i64>,
    pub visibility: String,
    pub added_at: i64,
    pub last_synced_at: Option<i64>,
}

// ── Frontend / Tauri IPC response types ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontendSyncResult {
    pub posts: Vec<Post>,
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

// ── Federated server HTTP API types ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub description: String,
    pub version: String,
    pub node_id: String,
    pub registered_users: i64,
    pub total_posts: i64,
    pub uptime_seconds: u64,
    pub registration_open: bool,
    #[serde(default)]
    pub retention_days: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFeedPost {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: i64,
    pub media_json: Option<String>,
    pub reply_to: Option<String>,
    pub reply_to_author: Option<String>,
    pub quote_of: Option<String>,
    pub quote_of_author: Option<String>,
    pub signature: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFeedResponse {
    pub posts: Vec<ServerFeedPost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendingHashtag {
    pub tag: String,
    pub post_count: i64,
    pub computed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendingResponse {
    pub hashtags: Vec<TrendingHashtag>,
    pub computed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerUser {
    pub pubkey: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_hash: Option<String>,
    pub visibility: String,
    pub registered_at: i64,
    pub post_count: i64,
    pub latest_post_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSearchResponse {
    pub users: Vec<ServerUser>,
    pub total: usize,
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSearchPost {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: i64,
    pub media_json: Option<String>,
    pub reply_to: Option<String>,
    pub reply_to_author: Option<String>,
    pub quote_of: Option<String>,
    pub quote_of_author: Option<String>,
    pub signature: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostSearchResponse {
    pub posts: Vec<ServerSearchPost>,
    pub total: i64,
    pub query: String,
}

/// Compact summary of local state for device sync negotiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSyncVector {
    pub post_count: u64,
    pub newest_post_ts: u64,
    pub interaction_count: u64,
    pub newest_interaction_ts: u64,
    /// Full follow list with LWW timestamps.
    pub follows: Vec<FollowEntry>,
    /// Full moderation list (mutes + blocks) with LWW timestamps.
    pub moderation: Vec<ModerationEntry>,
    /// All bookmark post IDs.
    pub bookmarks: Vec<String>,
    /// Ratchet session summaries (peer + updated_at).
    pub ratchet_summaries: Vec<RatchetSyncEntry>,
    /// Newest DM message timestamp across all conversations.
    pub dm_newest_ts: u64,
}
