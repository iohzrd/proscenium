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
    /// When the owner last changed this profile (Unix ms). Covered by the signature.
    /// A stored profile is replaced only by one with a greater or equal value.
    #[serde(default)]
    pub updated_at: u64,
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

/// Encoded in the QR code displayed by the existing device during linking.
#[derive(Clone, Serialize, Deserialize)]
pub struct LinkQrPayload {
    /// Existing device's iroh NodeId (for QUIC connection).
    pub node_id: String,
    /// One-time secret for the Noise PSK (32 bytes, URL-safe base64 without padding).
    pub secret: String,
    /// Existing device's relay URL for connection (if available).
    pub relay_url: Option<String>,
    /// Unix ms after which the existing device rejects this payload.
    pub expires_at: u64,
}

impl std::fmt::Debug for LinkQrPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkQrPayload")
            .field("node_id", &self.node_id)
            .field("relay_url", &self.relay_url)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Data sent from the existing device to the new device during linking.
#[derive(Clone, Serialize, Deserialize)]
pub struct LinkBundleData {
    /// The master secret key (32 bytes, standard base64).
    pub master_secret_key: String,
    /// The existing device's signed device list. Does not contain the new device.
    pub announcement: crate::protocol::LinkedDevicesAnnouncement,
    /// The user's signed profile.
    pub profile: Option<Profile>,
    /// Follow rows, including removed ones.
    pub follows: Vec<FollowEntry>,
    /// Mute and block rows, including removed ones.
    pub moderation: Vec<ModerationEntry>,
    /// Bookmark rows, including removed ones.
    pub bookmarks: Vec<BookmarkEntry>,
    /// Follow requests of other users with their state.
    pub follow_requests: Vec<FollowRequestEntry>,
    /// Signed device announcements of followed users. Each carries the user's transport
    /// NodeIds, which sync, gossip subscription and DMs require.
    pub peer_announcements: Vec<crate::protocol::LinkedDevicesAnnouncement>,
}

impl std::fmt::Debug for LinkBundleData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkBundleData")
            .field("devices", &self.announcement.devices.len())
            .field("follows", &self.follows.len())
            .field("moderation", &self.moderation.len())
            .field("bookmarks", &self.bookmarks.len())
            .field("peer_announcements", &self.peer_announcements.len())
            .finish_non_exhaustive()
    }
}

/// Follow entry with LWW state for device sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowEntry {
    pub pubkey: String,
    pub followed_at: u64,
    /// `"active"` or `"removed"`.
    pub state: String,
    pub last_changed_at: u64,
}

/// Moderation entry (mute or block) with LWW state for device sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationEntry {
    pub pubkey: String,
    /// `"mute"` or `"block"`.
    pub kind: String,
    pub created_at: u64,
    /// `"active"` or `"removed"`.
    pub state: String,
    pub last_changed_at: u64,
}

/// Bookmark entry with LWW state for device sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkEntry {
    pub post_id: String,
    pub created_at: u64,
    /// `"active"` or `"removed"`.
    pub state: String,
    pub last_changed_at: u64,
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
    /// When the request was approved or denied (Unix ms); 0 while pending.
    pub decided_at: u64,
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

impl ServerFeedPost {
    /// Rebuild the signed `Post` from the server's row.
    /// Returns `None` if `timestamp` is negative or `media_json` is not a list of attachments.
    pub fn to_post(&self) -> Option<Post> {
        let media = match &self.media_json {
            Some(json) => serde_json::from_str(json).ok()?,
            None => Vec::new(),
        };
        Some(Post {
            id: self.id.clone(),
            author: self.author.clone(),
            content: self.content.clone(),
            timestamp: u64::try_from(self.timestamp).ok()?,
            media,
            reply_to: self.reply_to.clone(),
            reply_to_author: self.reply_to_author.clone(),
            quote_of: self.quote_of.clone(),
            quote_of_author: self.quote_of_author.clone(),
            signature: self.signature.clone(),
        })
    }
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
    /// Transport NodeId of one of the user's devices, if the server holds one.
    #[serde(default)]
    pub transport_node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSearchResponse {
    pub users: Vec<ServerUser>,
    pub total: usize,
    pub query: String,
}

/// A post in a search response. The server sends the same row as in a feed response.
pub type ServerSearchPost = ServerFeedPost;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostSearchResponse {
    pub posts: Vec<ServerSearchPost>,
    pub total: i64,
    pub query: String,
}

/// Summary of one device's state, exchanged at the start of a device sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSyncVector {
    /// Hex SHA-256 over the sender's follow, moderation and bookmark rows
    /// (key, state, `last_changed_at`). Equal digests mean equal rows.
    pub lww_digest: String,
    /// Ids of the sender's newest direct messages, at most `MAX_DEVICE_SYNC_DM_IDS`.
    pub dm_ids: Vec<String>,
    /// Ids of the sender's newest deletions of own interactions, at most
    /// `MAX_SYNC_TOMBSTONES`.
    pub interaction_tombstone_ids: Vec<String>,
}

/// Number of direct message ids a `DeviceSyncVector` lists.
pub const MAX_DEVICE_SYNC_DM_IDS: usize = 50_000;
