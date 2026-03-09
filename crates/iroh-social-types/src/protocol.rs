use crate::delegation::SigningKeyDelegation;
use crate::signing::{hex_to_signature, signature_to_hex};
use crate::types::{Interaction, Post, Profile};
use iroh::{PublicKey, SecretKey};
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipMessage {
    NewPost(Post),
    DeletePost {
        id: String,
        author: String,
        signature: String,
    },
    ProfileUpdate(Profile),
    NewInteraction(Interaction),
    DeleteInteraction {
        id: String,
        author: String,
        signature: String,
    },
}

/// Response to an IdentityRequest. Contains everything a peer needs
/// to verify this user's identity and cache their delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityResponse {
    /// The user's permanent identity (master public key).
    pub master_pubkey: String,
    /// The current signing key delegation (signed by master key).
    pub delegation: SigningKeyDelegation,
    /// Transport NodeIds for this user's devices.
    pub transport_node_ids: Vec<String>,
    /// The user's profile, if available.
    pub profile: Option<Profile>,
}

pub fn user_feed_topic(pubkey: &str) -> TopicId {
    let mut hasher = Sha256::new();
    hasher.update(b"iroh-social-feed-v1:");
    hasher.update(pubkey.as_bytes());
    TopicId::from_bytes(hasher.finalize().into())
}

/// Single ALPN for all peer-to-peer protocol messages (sync, push, follow requests).
pub const PEER_ALPN: &[u8] = b"iroh-social/peer/1";

pub const MAX_PUSH_POSTS: usize = 50;
pub const MAX_PUSH_INTERACTIONS: usize = 200;

/// First message sent on a peer connection to identify intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerRequest {
    Sync(SyncRequest),
    Push(PushMessage),
    FollowRequest(FollowRequest),
    /// Ask a peer "who are you?" -- resolves transport NodeId to master pubkey.
    IdentityRequest,
}

/// Response sent back depending on the request type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerResponse {
    SyncSummary(SyncSummary),
    PushAck(PushAck),
    FollowResponse(FollowResponse),
    Identity(IdentityResponse),
}

/// Pushed from author to follower/mutual.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushMessage {
    pub author: String,
    pub posts: Vec<Post>,
    pub interactions: Vec<Interaction>,
    pub profile: Option<Profile>,
}

/// Acknowledgment from recipient.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushAck {
    pub received_post_ids: Vec<String>,
    pub received_interaction_ids: Vec<String>,
}

/// Follow request from one user to another (for Listed visibility).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowRequest {
    /// The requester's master pubkey (permanent identity).
    pub requester: String,
    pub timestamp: u64,
    /// Signed by the requester's signing key (verified via delegation).
    pub signature: String,
    /// The requester's signing key delegation, so the receiver can verify the signature.
    pub delegation: SigningKeyDelegation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FollowResponse {
    Approved,
    Denied,
    Pending,
}

/// Phase 1: Client sends summary of what it has for an author.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    pub author: String,
    pub post_count: u64,
    pub interaction_count: u64,
    /// Newest post timestamp the client has for this author (0 = no posts).
    pub newest_timestamp: u64,
    /// Newest interaction timestamp the client has for this author (0 = no interactions).
    pub newest_interaction_timestamp: u64,
}

/// Phase 1: Server responds with its counts and whether timestamp catch-up suffices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSummary {
    pub server_post_count: u64,
    pub server_interaction_count: u64,
    /// Number of posts the server has with timestamp > client's newest_timestamp.
    pub posts_after_count: u64,
    /// Number of interactions the server has with timestamp > client's newest_interaction_timestamp.
    pub interactions_after_count: u64,
    /// The sync mode the server will use for streaming.
    pub mode: SyncMode,
    pub profile: Option<Profile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMode {
    /// Counts match, nothing to send.
    UpToDate,
    /// Pure timestamp catch-up: stream posts with ts > client newest.
    TimestampCatchUp,
    /// ID diff required: client must send known IDs.
    NeedIdDiff,
}

/// Streamed frame over the QUIC bi-stream.
/// Length-prefixed: [4-byte big-endian len][JSON payload].
/// A zero-length frame signals end of stream.
#[derive(Debug, Serialize, Deserialize)]
pub enum SyncFrame {
    Posts(Vec<Post>),
    Interactions(Vec<Interaction>),
}

/// Canonical bytes for signing a follow request.
fn follow_request_signing_bytes(requester: &str, timestamp: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "requester": requester,
        "timestamp": timestamp,
    }))
    .expect("json serialization should not fail")
}

/// Sign a follow request.
pub fn sign_follow_request(requester: &str, timestamp: u64, secret_key: &SecretKey) -> String {
    let bytes = follow_request_signing_bytes(requester, timestamp);
    let sig = secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a follow request's signature against the given signer public key.
/// The signer is the signing key (from a cached delegation), NOT req.requester
/// (which is the master pubkey / permanent identity).
pub fn verify_follow_request(req: &FollowRequest, signer_pubkey: &PublicKey) -> Result<(), String> {
    let sig = hex_to_signature(&req.signature)?;
    let bytes = follow_request_signing_bytes(&req.requester, req.timestamp);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "follow request signature verification failed".to_string())
}
