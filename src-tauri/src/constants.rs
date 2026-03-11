use std::time::Duration;

// Startup sync
pub const RELAY_CHECK_INTERVAL: Duration = Duration::from_secs(1);
pub const RELAY_WAIT_ATTEMPTS: u32 = 10;
pub const PEER_READY_DELAY: Duration = Duration::from_secs(5);
pub const SYNC_TIMEOUT: Duration = Duration::from_secs(30);
pub const SYNC_MAX_RETRIES: u32 = 3;
pub const SYNC_CONCURRENCY: usize = 5;

// Drip sync
pub const DRIP_INITIAL_DELAY: Duration = Duration::from_secs(30);
pub const DRIP_PEER_PACE: Duration = Duration::from_secs(5);
pub const DRIP_ACTIVE_INTERVAL: Duration = Duration::from_secs(30);
pub const DRIP_IDLE_INTERVAL: Duration = Duration::from_secs(120);

// DM outbox
pub const OUTBOX_FLUSH_INTERVAL: Duration = Duration::from_secs(15);

// Push outbox
pub const PUSH_OUTBOX_FLUSH_INTERVAL: Duration = Duration::from_secs(30);
pub const PUSH_OUTBOX_PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

// Follow request expiry
pub const FOLLOW_REQUEST_PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

// Device sync
pub const DEVICE_SYNC_INTERVAL: Duration = Duration::from_secs(60);
pub const DEVICE_SYNC_INITIAL_DELAY: Duration = Duration::from_secs(15);

// Android network monitoring
#[cfg(target_os = "android")]
pub const ANDROID_NET_INTERVAL: Duration = Duration::from_secs(30);

// Sleep/wake detection
pub const WAKE_CHECK_INTERVAL: Duration = Duration::from_secs(10);
pub const WAKE_THRESHOLD: Duration = Duration::from_secs(30);

// Gossip heartbeat
pub const GOSSIP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

// Relay address logging delay
pub const RELAY_LOG_DELAY: Duration = Duration::from_secs(3);

// Defaults
pub const DEFAULT_FEED_LIMIT: usize = 20;
pub const DEFAULT_NOTIFICATION_LIMIT: usize = 30;
pub const DEFAULT_DM_LIMIT: usize = 50;
pub const DEFAULT_REPLY_LIMIT: u32 = 50;
