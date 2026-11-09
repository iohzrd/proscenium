use std::time::Duration;

// Sync protocol
/// Rows per database query and upper bound of items per sync frame.
pub const BATCH_SIZE: usize = 200;
/// Time allowed to connect to one NodeId for a sync.
pub const SYNC_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed for one sync step that is not a frame: opening or accepting a stream,
/// writing the request, reading the summary, transferring the id list.
pub const SYNC_STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound of the run time of one `sync_one_peer` call, over all NodeIds it connects to.
pub const SYNC_PEER_TIMEOUT: Duration = Duration::from_secs(300);
/// A responder splits a `Posts` frame whose JSON is larger than this.
pub const SYNC_FRAME_TARGET_BYTES: usize = 512 * 1024;
/// Largest sync frame a requester accepts.
pub const SYNC_MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Largest sync summary a requester accepts.
pub const SYNC_MAX_SUMMARY_BYTES: usize = 64 * 1024;
/// Upper bounds of items a requester accepts in one sync, over all frames.
pub const SYNC_MAX_POSTS_PER_RUN: usize = 100_000;
pub const SYNC_MAX_INTERACTIONS_PER_RUN: usize = 100_000;
/// Number of newly stored posts `sync_one_peer` returns to its caller (the newest ones).
pub const SYNC_RETURNED_POSTS_MAX: usize = 200;
/// Tombstones per `Tombstones` frame sent by a responder.
pub const SYNC_TOMBSTONE_BATCH: usize = 1_000;
/// Sync requests served at the same time.
pub const SYNC_SERVER_CONCURRENCY: usize = 2;

// Peer protocol (accept side)
/// Connections on the peer ALPN handled at the same time, in total and per remote NodeId.
pub const PEER_MAX_CONNECTIONS: usize = 64;
pub const PEER_MAX_CONNECTIONS_PER_NODE: usize = 4;
/// Time allowed for the remote to open the request stream.
pub const PEER_ACCEPT_STREAM_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed to receive one request body.
pub const PEER_REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Time allowed to write one response.
pub const PEER_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(15);
/// Time a handler waits for the remote to close the connection after the response.
pub const PEER_CLOSE_WAIT: Duration = Duration::from_secs(5);
/// Request body limits per request type.
pub const PEER_MAX_SMALL_REQUEST_BYTES: usize = 2 * 1024;
pub const PEER_MAX_FOLLOW_REQUEST_BYTES: usize = 64 * 1024;
pub const PEER_MAX_LINK_REQUEST_BYTES: usize = 1024;
pub const PEER_MAX_PUSH_BYTES: usize = 4 * 1024 * 1024;
pub const PEER_MAX_DEVICE_SYNC_REQUEST_BYTES: usize = 4 * 1024 * 1024;

// Peer protocol (connect side)
/// Time allowed for one identity, follow-request or list exchange (connect, request, response).
pub const PEER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Number of cached NodeIds of one user that a sync, push, list fetch or follow request
/// connects to.
pub const MAX_NODE_IDS_TRIED: usize = 4;
/// Entries stored per fetched follow or follower list.
pub const MAX_REMOTE_LIST_ENTRIES: usize = 5_000;
/// Largest list response a requester accepts.
pub const PEER_MAX_LIST_RESPONSE_BYTES: usize = 1_000_000;

// Push
pub const PUSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const PUSH_ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Push connections open at the same time, over all pending pushes.
pub const PUSH_CONCURRENCY: usize = 8;
/// Signed deletions accepted in one `PushMessage`.
pub const MAX_PUSH_TOMBSTONES: usize = 200;

// Follow requests
pub const FOLLOW_REQUEST_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000; // 30 days
/// Pending follow requests stored at the same time.
pub const MAX_PENDING_FOLLOW_REQUESTS: u64 = 500;

// Peer sync task
pub const RELAY_CHECK_INTERVAL: Duration = Duration::from_secs(1);
pub const RELAY_WAIT_ATTEMPTS: u32 = 10;
pub const PEER_READY_DELAY: Duration = Duration::from_secs(5);
pub const SYNC_CONCURRENCY: usize = 4;
pub const DRIP_IDLE_INTERVAL: Duration = Duration::from_secs(300); // 5 min
pub const DRIP_ACTIVE_INTERVAL: Duration = Duration::from_secs(60); // 1 min after activity

// Device sync task
pub const DEVICE_SYNC_INITIAL_DELAY: Duration = Duration::from_secs(15);
pub const DEVICE_SYNC_INTERVAL: Duration = Duration::from_secs(300); // 5 min

// DM outbox
pub const OUTBOX_FLUSH_INTERVAL: Duration = Duration::from_secs(60);
/// Delay between the first and the second delivery attempt of a queued message.
/// The delay doubles after each further failed attempt.
pub const OUTBOX_RETRY_BASE: Duration = Duration::from_secs(30);
/// Upper bound of the delay between two delivery attempts.
pub const OUTBOX_RETRY_MAX: Duration = Duration::from_secs(15 * 60);
/// A queued message older than this is discarded without further attempts.
pub const OUTBOX_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Maximum number of queued messages one flush pass attempts.
pub const OUTBOX_PASS_LIMIT: u32 = 200;
/// Time allowed for one delivery of a DM payload to the devices of the recipient: connect,
/// write and acknowledgement for each device.
pub const DM_SEND_TIMEOUT: Duration = Duration::from_secs(60);
/// Time `send_dm` waits for the first delivery attempt before it returns
/// `delivered: false`. The attempt continues after that.
pub const DM_SEND_WAIT: Duration = Duration::from_secs(10);
/// Time added to `DM_SEND_TIMEOUT` before the flush loop may select a new outbox entry,
/// so the first attempt has recorded its result by then.
pub const OUTBOX_FIRST_ATTEMPT_MARGIN: Duration = Duration::from_secs(15);

// Gossip
/// Maximum size of one gossip frame. A serialized post (10,000 content bytes plus ten
/// attachments with tickets of up to 2,048 bytes each) must fit.
pub const GOSSIP_MAX_MESSAGE_SIZE: usize = 64 * 1024;
/// Bytes of a gossip frame reserved for the iroh-gossip header; a payload larger than
/// `GOSSIP_MAX_MESSAGE_SIZE - GOSSIP_FRAME_OVERHEAD` is not broadcast.
pub const GOSSIP_FRAME_OVERHEAD: usize = 1024;
/// Interval at which a subscription without neighbors calls `join_peers` again.
pub const GOSSIP_REJOIN_INTERVAL: Duration = Duration::from_secs(30);
/// First and largest delay before a failed subscription is attempted again.
pub const GOSSIP_RECONNECT_BASE_DELAY: Duration = Duration::from_secs(5);
pub const GOSSIP_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(300);

// Own feed (followers joining the local user's topic)
/// Identity queries for new neighbors running at the same time, and neighbors waiting for one.
pub const OWN_FEED_RESOLVE_CONCURRENCY: usize = 8;
pub const OWN_FEED_RESOLVE_QUEUE: usize = 64;
/// New follower rows (and "follower" notifications) recorded per window.
pub const OWN_FEED_NEW_FOLLOWERS_PER_WINDOW: u32 = 30;
pub const OWN_FEED_FOLLOWER_WINDOW: Duration = Duration::from_secs(600);

// Network health (gossip)
pub const HEALTH_TICK_INTERVAL: Duration = Duration::from_secs(5);
pub const WAKE_THRESHOLD: Duration = Duration::from_secs(15);

// Android network
#[cfg(target_os = "android")]
pub const ANDROID_NET_INTERVAL: Duration = Duration::from_secs(30);

// Storage query defaults
pub const DEFAULT_FEED_LIMIT: usize = 20;
pub const DEFAULT_NOTIFICATION_LIMIT: usize = 30;
pub const DEFAULT_DM_LIMIT: usize = 50;
pub const DEFAULT_REPLY_LIMIT: u32 = 50;
