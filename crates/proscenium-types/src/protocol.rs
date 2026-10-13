use crate::delegation::{SigningKeyDelegation, SigningKeyRotation};
use crate::signing::{
    VerifiedIdentity, canonical_bytes, hex_to_signature, signature_to_hex, verify_identity_response,
};
use crate::stage::StageTicket;
use crate::types::{
    BookmarkEntry, DeviceEntry, DeviceSyncVector, FollowEntry, Interaction, ModerationEntry, Post,
    Profile, SocialGraphEntry,
};
use crate::validation::is_recent_timestamp;
use iroh::{PublicKey, SecretKey};
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum number of devices in one `LinkedDevicesAnnouncement`.
pub const MAX_LINKED_DEVICES: usize = 16;
/// Maximum byte length of a device name in a `LinkedDevicesAnnouncement`.
pub const MAX_DEVICE_NAME_LEN: usize = 64;

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
    /// Host announces a new live audio Stage room on their feed topic.
    /// `signature` is made by the host's signing key; see `verify_stage_announcement`.
    StageAnnouncement {
        stage_id: String,
        title: String,
        ticket: crate::stage::StageTicket,
        host_pubkey: String,
        started_at: u64,
        signature: String,
    },
    /// Host signals the Stage room has ended.
    /// `signature` is made by the host's signing key; see `verify_stage_ended`.
    StageEnded {
        stage_id: String,
        host_pubkey: String,
        signature: String,
    },
}

/// Response to an IdentityRequest.
///
/// Unauthenticated until passed through `verify_identity_response`, which checks the
/// delegation, the device list signature, that the sending NodeId is in the device list,
/// and the profile signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityResponse {
    /// Signed device list. Carries the master pubkey and the current delegation.
    pub announcement: LinkedDevicesAnnouncement,
    /// The user's signed profile, if one exists.
    pub profile: Option<Profile>,
}

pub fn user_feed_topic(pubkey: &str) -> TopicId {
    let mut hasher = Sha256::new();
    hasher.update(b"proscenium-feed-v2:");
    hasher.update(pubkey.as_bytes());
    TopicId::from_bytes(hasher.finalize().into())
}

/// Single ALPN for all peer-to-peer protocol messages (sync, push, follow requests).
pub const PEER_ALPN: &[u8] = b"proscenium/peer/2";

pub const MAX_PUSH_POSTS: usize = 50;
pub const MAX_PUSH_INTERACTIONS: usize = 200;

/// First message sent on a peer connection to identify intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerRequest {
    Sync(SyncRequest),
    Push(Box<PushMessage>),
    FollowRequest(Box<FollowRequest>),
    /// Ask a peer "who are you?" -- resolves transport NodeId to master pubkey.
    IdentityRequest,
    /// Request a peer's public follow list.
    FollowsListRequest,
    /// Request a peer's public followers list.
    FollowersListRequest,
    /// Device linking: the new device sends the first Noise IKpsk2 handshake message.
    LinkRequest {
        /// Noise IKpsk2 handshake init message (opaque bytes).
        noise_init: Vec<u8>,
    },
    /// Sync between two devices of one user. The responder serves it only to a NodeId
    /// derived from its own master key. `DeviceSyncFrame`s follow on a second
    /// bi-stream opened by the requester.
    DeviceSyncRequest,
}

/// Response sent back depending on the request type.
///
/// Sync, push and follow requests are answered with a bare `SyncSummary`, `PushAck` and
/// `FollowResponse`; they have no variant here.
// One value exists per request and is serialized at once, so the size of the
// `Identity` variant is not boxed away.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerResponse {
    Identity(IdentityResponse),
    /// Device linking: the existing device's Noise IKpsk2 handshake response.
    /// The `LinkBundleData`, encrypted as a sequence of Noise transport messages,
    /// follows on the same stream.
    LinkBundle {
        /// Noise IKpsk2 handshake response message (opaque bytes).
        noise_response: Vec<u8>,
    },
    /// Response containing a peer's follows list.
    FollowsList(FollowsListResponse),
    /// Response containing a peer's followers list.
    FollowersList(FollowersListResponse),
}

/// Pushed from author to follower/mutual.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushMessage {
    pub author: String,
    pub posts: Vec<Post>,
    pub interactions: Vec<Interaction>,
    pub profile: Option<Profile>,
    /// The author's signed device list. Carries the current delegation, so a receiver
    /// that missed a signing key rotation can verify the pushed content.
    #[serde(default)]
    pub announcement: Option<LinkedDevicesAnnouncement>,
    /// Signed deletions of the author's posts.
    #[serde(default)]
    pub tombstones: Vec<PostTombstone>,
}

/// A signed deletion of one post. `signature` is the output of `sign_delete_post`
/// and is verified with `verify_delete_post_signature`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostTombstone {
    pub id: String,
    pub author: String,
    pub signature: String,
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
    /// The master pubkey of the user being asked. Signed, so a request for one
    /// user is rejected by every other user.
    pub target: String,
    /// When the request was created (Unix ms). Signed; receivers reject values
    /// further than `MAX_TIMESTAMP_DRIFT_MS` from their clock.
    pub timestamp: u64,
    /// Signed by the requester's signing key over `(requester, target, timestamp)`.
    pub signature: String,
    /// The requester's identity. Its signed device list must contain the sending NodeId.
    pub identity: IdentityResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FollowResponse {
    Approved(Box<IdentityResponse>),
    Denied,
    Pending,
}

/// Response to a FollowsListRequest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowsListResponse {
    pub pubkey: String,
    pub follows: Vec<SocialGraphEntry>,
    /// True if the user has hidden their follow list.
    pub hidden: bool,
}

/// Response to a FollowersListRequest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowersListResponse {
    pub pubkey: String,
    pub followers: Vec<SocialGraphEntry>,
    /// True if the user has hidden their followers list.
    pub hidden: bool,
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
    /// `post_ids_digest` over the post ids the client stores for this author.
    pub post_ids_digest: String,
    /// Number of deletion records the client stores for this author.
    pub tombstone_count: u64,
}

/// Maximum number of post ids a requester may send in the id-diff phase.
pub const MAX_SYNC_IDS: usize = 50_000;
/// Maximum number of tombstones a responder streams in one sync.
pub const MAX_SYNC_TOMBSTONES: usize = 10_000;

/// Hex SHA-256 over `ids` sorted ascending, each followed by a newline.
/// Two peers hold the same set of post ids exactly when their digests are equal.
pub fn post_ids_digest(ids: &mut [String]) -> String {
    ids.sort_unstable();
    let mut hasher = Sha256::new();
    for id in ids.iter() {
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

/// A live stage announcement, included in sync responses so peers that
/// come online after the gossip broadcast can still discover active stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageAnnouncement {
    pub stage_id: String,
    pub title: String,
    pub ticket: StageTicket,
    pub host_pubkey: String,
    pub started_at: u64,
    /// Signature by the host's signing key; see `verify_stage_announcement`.
    #[serde(default)]
    pub signature: String,
}

const STAGE_ANNOUNCEMENT_TAG: &str = "proscenium/stage-announcement/2";
const STAGE_ENDED_TAG: &str = "proscenium/stage-ended/2";

fn stage_announcement_signing_bytes(
    stage_id: &str,
    title: &str,
    ticket: &StageTicket,
    host_pubkey: &str,
    started_at: u64,
) -> Vec<u8> {
    canonical_bytes(&(
        STAGE_ANNOUNCEMENT_TAG,
        stage_id,
        title,
        ticket.to_string(),
        host_pubkey,
        started_at,
    ))
}

/// Sign a stage announcement with the host's signing key. `host_pubkey` is the host's master pubkey.
pub fn sign_stage_announcement(
    stage_id: &str,
    title: &str,
    ticket: &StageTicket,
    host_pubkey: &str,
    started_at: u64,
    secret_key: &SecretKey,
) -> String {
    let bytes = stage_announcement_signing_bytes(stage_id, title, ticket, host_pubkey, started_at);
    signature_to_hex(&secret_key.sign(&bytes))
}

/// Verify a stage announcement against the host's signing key.
/// The caller must also check that `host_pubkey` is the identity the signing key belongs to.
pub fn verify_stage_announcement(
    stage_id: &str,
    title: &str,
    ticket: &StageTicket,
    host_pubkey: &str,
    started_at: u64,
    signature: &str,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(signature)?;
    let bytes = stage_announcement_signing_bytes(stage_id, title, ticket, host_pubkey, started_at);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "stage announcement signature verification failed".to_string())
}

/// Sign a stage-ended message with the host's signing key.
pub fn sign_stage_ended(stage_id: &str, host_pubkey: &str, secret_key: &SecretKey) -> String {
    let bytes = canonical_bytes(&(STAGE_ENDED_TAG, stage_id, host_pubkey));
    signature_to_hex(&secret_key.sign(&bytes))
}

/// Verify a stage-ended message against the host's signing key.
pub fn verify_stage_ended(
    stage_id: &str,
    host_pubkey: &str,
    signature: &str,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(signature)?;
    let bytes = canonical_bytes(&(STAGE_ENDED_TAG, stage_id, host_pubkey));
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "stage ended signature verification failed".to_string())
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
    /// If the peer is currently hosting a live stage, included here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_stage: Option<StageAnnouncement>,
    /// The responder's signed device list. Carries the current delegation, so a requester
    /// that missed a signing key rotation can verify the synced content.
    #[serde(default)]
    pub announcement: Option<LinkedDevicesAnnouncement>,
    /// Number of deletion records the responder stores for the requested author.
    #[serde(default)]
    pub server_tombstone_count: u64,
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
    /// Signed deletions of the author's posts.
    Tombstones(Vec<PostTombstone>),
}

/// Streamed frame for device-to-device sync.
/// Uses the same length-prefixed framing as SyncFrame.
#[derive(Debug, Serialize, Deserialize)]
pub enum DeviceSyncFrame {
    /// The sender's state summary. First frame in each direction.
    Vector(DeviceSyncVector),
    Posts(Vec<Post>),
    Interactions(Vec<Interaction>),
    Follows(Vec<FollowEntry>),
    Moderation(Vec<ModerationEntry>),
    Bookmarks(Vec<BookmarkEntry>),
}

const FOLLOW_REQUEST_TAG: &str = "proscenium/follow-request/2";

/// Canonical bytes for signing a follow request.
fn follow_request_signing_bytes(requester: &str, target: &str, timestamp: u64) -> Vec<u8> {
    canonical_bytes(&(FOLLOW_REQUEST_TAG, requester, target, timestamp))
}

/// Sign a follow request from `requester` to `target` (both master pubkeys).
pub fn sign_follow_request(
    requester: &str,
    target: &str,
    timestamp: u64,
    secret_key: &SecretKey,
) -> String {
    let bytes = follow_request_signing_bytes(requester, target, timestamp);
    let sig = secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a follow request received from `remote_node_id` by the user `my_master_pubkey`.
///
/// Fails unless: `target` is `my_master_pubkey`, the timestamp is within
/// `MAX_TIMESTAMP_DRIFT_MS` of the local clock, the embedded identity verifies and lists
/// `remote_node_id`, the identity's master pubkey equals `requester`, and the request
/// signature was made by the delegated signing key.
pub fn verify_follow_request(
    req: &FollowRequest,
    remote_node_id: &str,
    my_master_pubkey: &str,
) -> Result<VerifiedIdentity, String> {
    if req.target != my_master_pubkey {
        return Err("follow request is addressed to another user".to_string());
    }
    if !is_recent_timestamp(req.timestamp) {
        return Err("follow request timestamp is outside the accepted window".to_string());
    }
    let identity = verify_identity_response(req.identity.clone(), remote_node_id)?;
    if identity.master_pubkey != req.requester {
        return Err("follow request requester does not match its identity".to_string());
    }
    let signer: PublicKey = identity
        .delegation()
        .signing_pubkey
        .parse()
        .map_err(|e| format!("invalid signing pubkey in delegation: {e}"))?;
    let sig = hex_to_signature(&req.signature)?;
    let bytes = follow_request_signing_bytes(&req.requester, &req.target, req.timestamp);
    signer
        .verify(&bytes, &sig)
        .map_err(|_| "follow request signature verification failed".to_string())?;
    Ok(identity)
}
