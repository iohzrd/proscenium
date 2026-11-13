use crate::dm::StoredMessage;
use crate::signing::{
    VerifiedIdentity, canonical_bytes, signature_to_hex, verify_identity_response, verify_signed_by,
};
use crate::stage::StageTicket;
use crate::types::{
    BookmarkEntry, DeviceEntry, DeviceSyncVector, FollowEntry, FollowRequestEntry, Interaction,
    ModerationEntry, Post, Profile, SocialGraphEntry,
};
use crate::validation::is_recent_timestamp;
use iroh::SecretKey;
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum number of devices in one `LinkedDevicesAnnouncement`.
pub const MAX_LINKED_DEVICES: usize = 16;
/// Maximum number of removed NodeIds in one `LinkedDevicesAnnouncement`.
pub const MAX_REMOVED_DEVICES: usize = 16;
/// Maximum byte length of a device name in a `LinkedDevicesAnnouncement`.
pub const MAX_DEVICE_NAME_LEN: usize = 64;
/// Maximum byte length of the JSON of a `LinkedDevicesAnnouncement` that passes
/// `verify_linked_devices_announcement`: `MAX_LINKED_DEVICES` entries whose names of
/// `MAX_DEVICE_NAME_LEN` bytes are escaped as `\u00XX` (6 bytes per byte),
/// `MAX_REMOVED_DEVICES` NodeIds, and under 1 KiB of other fields.
pub const MAX_ANNOUNCEMENT_JSON_LEN: usize =
    MAX_LINKED_DEVICES * (6 * MAX_DEVICE_NAME_LEN + 256) + MAX_REMOVED_DEVICES * 128 + 1024;

/// Signed announcement listing all of a user's linked devices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedDevicesAnnouncement {
    /// The user's identity (master public key).
    pub master_pubkey: String,
    /// All currently active devices.
    pub devices: Vec<DeviceEntry>,
    /// NodeIds of devices removed from the identity, newest first. None is in `devices`.
    /// A device that finds its own NodeId here deletes its data.
    pub removed: Vec<String>,
    /// Creation time (Unix timestamp ms). Receivers keep the announcement with the
    /// highest version.
    pub version: u64,
    /// Ed25519 signature by the master key over the canonical bytes.
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
    /// Lightweight keep-alive so followers know the connection is still
    /// active even when the publisher isn't posting.
    Heartbeat,
    /// Host announces a new live audio Stage room on their feed topic.
    /// `signature` is made by the host's master key; see `verify_stage_announcement`.
    StageAnnouncement {
        stage_id: String,
        title: String,
        ticket: crate::stage::StageTicket,
        host_pubkey: String,
        started_at: u64,
        signature: String,
    },
    /// Host signals the Stage room has ended.
    /// `signature` is made by the host's master key; see `verify_stage_ended`.
    StageEnded {
        stage_id: String,
        host_pubkey: String,
        signature: String,
    },
}

/// Response to an IdentityRequest.
///
/// Unauthenticated until passed through `verify_identity_response`, which checks the
/// device list signature, that the sending NodeId is in the device list, and the profile
/// signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityResponse {
    /// Signed device list. Carries the master pubkey.
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
    /// Device linking: the new device sends the one-time secret of the link code. After
    /// the user confirmed the link code on it, the existing device answers with the
    /// `LinkBundleData` JSON on the same stream.
    LinkRequest {
        /// `LinkQrPayload::secret` (URL-safe base64 without padding).
        secret: String,
        /// NodeId of the transport key that the new device generated for the identity.
        node_id: String,
        /// Name under which the existing device stores the new device.
        device_name: String,
    },
    /// Sync between two devices of one user. The responder serves it only if
    /// `announcement` is signed by its own master key and lists the requester's NodeId,
    /// and answers with its own announcement on the same stream. `DeviceSyncFrame`s
    /// follow on a second bi-stream opened by the requester.
    DeviceSyncRequest {
        /// The requester's signed device list.
        announcement: LinkedDevicesAnnouncement,
    },
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
    /// The author's signed device list, so a receiver that missed a device list change
    /// learns the current NodeIds.
    #[serde(default)]
    pub announcement: Option<LinkedDevicesAnnouncement>,
    /// Signed deletions of the author's posts.
    #[serde(default)]
    pub tombstones: Vec<PostTombstone>,
    /// A live stage of the author, signed. Hosts that are not Public announce stages by
    /// push, as they have no gossip topic.
    #[serde(default)]
    pub stage: Option<StageAnnouncement>,
    /// The end of a stage of the author, signed.
    #[serde(default)]
    pub stage_ended: Option<StageEnd>,
}

/// The end of a stage, signed by its host: `signature` is the output of
/// `sign_stage_ended` and is verified with `verify_stage_ended`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageEnd {
    pub stage_id: String,
    pub host_pubkey: String,
    pub signature: String,
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
    /// Signed by the requester's master key over `(requester, target, timestamp)`.
    pub signature: String,
    /// The requester's identity. Its signed device list must contain the sending NodeId.
    pub identity: IdentityResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FollowResponse {
    Approved(Box<IdentityResponse>),
    Denied,
    Pending,
    /// The receiver stores no new follow request now (rate limit). Nothing was stored.
    RateLimited,
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
    /// Earliest timestamp of the posts and interactions the requester stores. The
    /// responder counts and sends only posts and interactions with a timestamp at or
    /// after it, and the counts and digest above cover only those. 0 for all.
    pub since: u64,
    /// The requester's signed device list. A responder whose visibility requires a known
    /// requester uses it to map the requesting NodeId to its master. `None` from the
    /// discovery server.
    pub announcement: Option<LinkedDevicesAnnouncement>,
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
    /// Signature by the host's master key; see `verify_stage_announcement`.
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

/// Sign a stage announcement with the host's master key. `host_pubkey` is the host's
/// master pubkey.
pub fn sign_stage_announcement(
    stage_id: &str,
    title: &str,
    ticket: &StageTicket,
    host_pubkey: &str,
    started_at: u64,
    master_key: &SecretKey,
) -> String {
    let bytes = stage_announcement_signing_bytes(stage_id, title, ticket, host_pubkey, started_at);
    signature_to_hex(&master_key.sign(&bytes))
}

/// Oldest stage start time accepted in an announcement, relative to the local clock.
pub const MAX_STAGE_ANNOUNCEMENT_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// Verify a stage announcement: the ticket passes `StageTicket::verify` and names the same
/// stage, host and title as the announcement, `started_at` is at most
/// `MAX_TIMESTAMP_DRIFT_MS` in the future and at most `MAX_STAGE_ANNOUNCEMENT_AGE_MS` in
/// the past, and `signature` was made by the key `host_pubkey`.
pub fn verify_stage_announcement(
    stage_id: &str,
    title: &str,
    ticket: &StageTicket,
    host_pubkey: &str,
    started_at: u64,
    signature: &str,
) -> Result<(), String> {
    ticket.verify()?;
    if ticket.stage_id != stage_id || ticket.host_pubkey != host_pubkey || ticket.title != title {
        return Err("stage ticket differs from the announcement".to_string());
    }
    let now = crate::validation::now_millis();
    if started_at > now.saturating_add(crate::validation::MAX_TIMESTAMP_DRIFT_MS)
        || started_at < now.saturating_sub(MAX_STAGE_ANNOUNCEMENT_AGE_MS)
    {
        return Err("stage start time is outside the accepted range".to_string());
    }
    verify_signed_by(
        host_pubkey,
        &stage_announcement_signing_bytes(stage_id, title, ticket, host_pubkey, started_at),
        signature,
        "stage announcement",
    )
}

/// Sign a stage-ended message with the host's master key.
pub fn sign_stage_ended(stage_id: &str, host_pubkey: &str, master_key: &SecretKey) -> String {
    let bytes = canonical_bytes(&(STAGE_ENDED_TAG, stage_id, host_pubkey));
    signature_to_hex(&master_key.sign(&bytes))
}

/// Verify that a stage-ended `signature` was made by the key `host_pubkey`.
pub fn verify_stage_ended(
    stage_id: &str,
    host_pubkey: &str,
    signature: &str,
) -> Result<(), String> {
    verify_signed_by(
        host_pubkey,
        &canonical_bytes(&(STAGE_ENDED_TAG, stage_id, host_pubkey)),
        signature,
        "stage ended",
    )
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
    /// The responder's signed device list, so a requester that missed a device list change
    /// learns the current NodeIds.
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

/// Interaction rule of the sync protocol, for a request whose interaction count differs
/// from the responder's: `true` if the responder sends only the interactions after the
/// requester's newest interaction timestamp, `false` if it sends every interaction.
///
/// The responder evaluates it to select the rows. The requester evaluates it to
/// determine whether the frames carry the author's complete interaction set; if they do,
/// the requester deletes the author's interactions that the set lacks.
pub fn sends_only_newer_interactions(
    responder_count: u64,
    requester_count: u64,
    responder_count_after_newest: u64,
) -> bool {
    responder_count.checked_sub(requester_count) == Some(responder_count_after_newest)
}

/// Streamed frame for device-to-device sync.
/// Uses the same length-prefixed framing as SyncFrame.
#[derive(Debug, Serialize, Deserialize)]
pub enum DeviceSyncFrame {
    /// The sender's state summary. First frame in each direction.
    Vector(DeviceSyncVector),
    Follows(Vec<FollowEntry>),
    /// Signed device lists of the users the sender follows. Sent with the follow rows.
    PeerAnnouncements(Vec<LinkedDevicesAnnouncement>),
    Moderation(Vec<ModerationEntry>),
    Bookmarks(Vec<BookmarkEntry>),
    /// Follow requests of other users with their state.
    FollowRequests(Vec<FollowRequestEntry>),
    /// Direct messages the receiver's vector does not list.
    DirectMessages(Vec<StoredMessage>),
    /// Ids of deleted own interactions the receiver's vector does not list.
    InteractionTombstones(Vec<String>),
}

const FOLLOW_REQUEST_TAG: &str = "proscenium/follow-request/2";

/// Canonical bytes for signing a follow request.
fn follow_request_signing_bytes(requester: &str, target: &str, timestamp: u64) -> Vec<u8> {
    canonical_bytes(&(FOLLOW_REQUEST_TAG, requester, target, timestamp))
}

/// Sign a follow request from `requester` to `target` (both master pubkeys) with the
/// requester's master key.
pub fn sign_follow_request(
    requester: &str,
    target: &str,
    timestamp: u64,
    master_key: &SecretKey,
) -> String {
    let bytes = follow_request_signing_bytes(requester, target, timestamp);
    let sig = master_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a follow request received from `remote_node_id` by the user `my_master_pubkey`.
///
/// Fails unless: `target` is `my_master_pubkey`, the timestamp is within
/// `MAX_TIMESTAMP_DRIFT_MS` of the local clock, the embedded identity verifies and lists
/// `remote_node_id`, the identity's master pubkey equals `requester`, and the request
/// signature was made by the key `requester`.
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
    verify_signed_by(
        &req.requester,
        &follow_request_signing_bytes(&req.requester, &req.target, req.timestamp),
        &req.signature,
        "follow request",
    )?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_newer_interactions_when_they_account_for_the_difference() {
        // The responder has 3 more, all after the requester's newest timestamp.
        assert!(super::sends_only_newer_interactions(10, 7, 3));
        // The responder has 3 more but only 2 are newer: an older one is missing.
        assert!(!super::sends_only_newer_interactions(10, 7, 2));
        // The requester has more than the responder: some were deleted by the author.
        assert!(!super::sends_only_newer_interactions(5, 7, 0));
        assert!(!super::sends_only_newer_interactions(0, u64::MAX, 0));
    }
}
