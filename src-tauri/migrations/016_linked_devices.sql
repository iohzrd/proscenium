-- Device registry (own linked devices)
CREATE TABLE IF NOT EXISTS linked_devices (
    node_id TEXT PRIMARY KEY,
    device_name TEXT NOT NULL,
    is_primary INTEGER NOT NULL DEFAULT 0,
    is_self INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL DEFAULT 0
);

-- Cached device announcements for peers (peer device discovery)
CREATE TABLE IF NOT EXISTS peer_device_announcements (
    master_pubkey TEXT PRIMARY KEY,
    announcement_json TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,
    cached_at INTEGER NOT NULL
);

-- LWW state columns for cross-device sync
ALTER TABLE follows ADD COLUMN state TEXT NOT NULL DEFAULT 'active';
ALTER TABLE follows ADD COLUMN last_changed_at INTEGER NOT NULL DEFAULT 0;

ALTER TABLE mutes ADD COLUMN state TEXT NOT NULL DEFAULT 'active';
ALTER TABLE mutes ADD COLUMN last_changed_at INTEGER NOT NULL DEFAULT 0;

ALTER TABLE blocks ADD COLUMN state TEXT NOT NULL DEFAULT 'active';
ALTER TABLE blocks ADD COLUMN last_changed_at INTEGER NOT NULL DEFAULT 0;
