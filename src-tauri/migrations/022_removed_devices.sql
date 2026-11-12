-- NodeIds removed from the local identity. A removed NodeId is not stored as a device again.
CREATE TABLE IF NOT EXISTS removed_devices (
    node_id TEXT PRIMARY KEY NOT NULL,
    removed_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_removed_devices_time ON removed_devices(removed_at DESC);
