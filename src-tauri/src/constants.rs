use std::time::Duration;

// Sync protocol
pub const SYNC_TIMEOUT: Duration = Duration::from_secs(30);
pub const BATCH_SIZE: usize = 200;

// Peer sync task
pub const RELAY_CHECK_INTERVAL: Duration = Duration::from_secs(1);
pub const RELAY_WAIT_ATTEMPTS: u32 = 10;
pub const PEER_READY_DELAY: Duration = Duration::from_secs(5);
pub const SYNC_CONCURRENCY: usize = 4;
pub const DRIP_IDLE_INTERVAL: Duration = Duration::from_secs(300); // 5 min
pub const DRIP_PEER_PACE: Duration = Duration::from_millis(500);
pub const DRIP_ACTIVE_INTERVAL: Duration = Duration::from_secs(60); // 1 min after activity

// Device sync task
pub const DEVICE_SYNC_INITIAL_DELAY: Duration = Duration::from_secs(15);
pub const DEVICE_SYNC_INTERVAL: Duration = Duration::from_secs(300); // 5 min

// DM outbox
pub const OUTBOX_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

// Network health (gossip)
pub const GOSSIP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
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
