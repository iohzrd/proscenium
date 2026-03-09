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
pub struct LinkPreview {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub site_name: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowEntry {
    pub pubkey: String,
    pub alias: Option<String>,
    pub followed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowerEntry {
    pub pubkey: String,
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
    pub follows: Vec<FollowEntry>,
    /// Bookmarked post IDs.
    pub bookmarks: Vec<String>,
    /// Blocked user pubkeys.
    pub blocked_users: Vec<String>,
    /// Muted user pubkeys.
    pub muted_users: Vec<String>,
    /// Current DM ratchet sessions (serialized).
    pub ratchet_sessions: Vec<RatchetSessionExport>,
}

/// Exported ratchet session for transfer during device pairing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetSessionExport {
    pub peer_pubkey: String,
    pub state_json: String,
}
