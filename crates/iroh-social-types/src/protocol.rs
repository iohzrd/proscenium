use crate::delegation::{SigningKeyDelegation, SigningKeyRotation};
use crate::signing::{hex_to_signature, signature_to_hex};
use crate::types::{
    DeviceEntry, DeviceSyncVector, FollowSyncEntry, Interaction, ModerationSyncEntry, Post,
    Profile, RatchetSessionExport,
};
use iroh::{PublicKey, SecretKey};
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Signed announcement listing all of a user's linked devices.
/// Signed by the signing key (NOT master key), verified via the included delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedDevicesAnnouncement {
    /// The user's permanent identity (master public key).
    pub master_pubkey: String,
    /// Current signing key delegation (signed by master key).
    pub delegation: SigningKeyDelegation,
    /// All currently active devices.
    pub devices: Vec<DeviceEntry>,
    /// Monotonically increasing version number.
    pub version: u64,
    /// When this announcement was created (Unix timestamp ms).
    pub timestamp: u64,
    /// Ed25519 signature from signing key over canonical bytes.
    pub signature: String,
}

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
    LinkedDevices(LinkedDevicesAnnouncement),
    SigningKeyRotation(SigningKeyRotation),
    /// Lightweight keep-alive so followers know the connection is still
    /// active even when the publisher isn't posting.
    Heartbeat,
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
    /// Device pairing: new device sends Noise IK+PSK init message.
    LinkRequest {
        /// Noise IK+PSK handshake init message (opaque bytes).
        noise_init: Vec<u8>,
    },
    /// Device-to-device sync: linked device requests state sync.
    DeviceSyncRequest {
        /// Random challenge (32 bytes) to prove signing key possession.
        challenge: Vec<u8>,
        /// Signature of the challenge with the signing key.
        challenge_sig: String,
        /// Compact summary of the initiator's local state.
        vector: DeviceSyncVector,
    },
}

/// Response sent back depending on the request type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerResponse {
    SyncSummary(SyncSummary),
    PushAck(PushAck),
    FollowResponse(FollowResponse),
    Identity(IdentityResponse),
    /// Device pairing: existing device sends Noise response + encrypted bundle.
    LinkBundle {
        /// Noise IK+PSK handshake response message (opaque bytes).
        noise_response: Vec<u8>,
        /// LinkBundleData encrypted with the Noise transport.
        encrypted_bundle: Vec<u8>,
    },
    /// Device-to-device sync: responder accepts and sends its own vector.
    DeviceSyncAccepted {
        /// Signature of the initiator's challenge (proves signing key).
        challenge_response: String,
        /// Responder's own sync vector.
        vector: DeviceSyncVector,
    },
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
    DeviceAnnouncements(Vec<LinkedDevicesAnnouncement>),
}

/// Streamed frame for device-to-device sync.
/// Uses the same length-prefixed framing as SyncFrame.
#[derive(Debug, Serialize, Deserialize)]
pub enum DeviceSyncFrame {
    Posts(Vec<Post>),
    Interactions(Vec<Interaction>),
    Follows(Vec<FollowSyncEntry>),
    Mutes(Vec<ModerationSyncEntry>),
    Blocks(Vec<ModerationSyncEntry>),
    Bookmarks(Vec<String>),
    RatchetSessions(Vec<RatchetSessionExport>),
}

/// Canonical bytes for signing a device sync challenge.
fn device_sync_challenge_signing_bytes(challenge: &[u8]) -> Vec<u8> {
    let mut bytes = b"iroh-social-device-sync-v1:".to_vec();
    bytes.extend_from_slice(challenge);
    bytes
}

/// Sign a device sync challenge with the signing key.
pub fn sign_device_sync_challenge(challenge: &[u8], secret_key: &SecretKey) -> String {
    let bytes = device_sync_challenge_signing_bytes(challenge);
    let sig = secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a device sync challenge signature against the signing key.
pub fn verify_device_sync_challenge(
    challenge: &[u8],
    signature: &str,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(signature)?;
    let bytes = device_sync_challenge_signing_bytes(challenge);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "device sync challenge verification failed".to_string())
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
