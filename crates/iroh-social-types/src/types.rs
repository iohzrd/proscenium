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
