-- NodeIds to which this device sent a link bundle, until their first device sync.
-- Device sync stores an unknown NodeId of the local identity only if it is listed here or
-- in the list of a stored device.
CREATE TABLE IF NOT EXISTS pending_link_devices (
    node_id TEXT PRIMARY KEY NOT NULL,
    linked_at INTEGER NOT NULL
);
