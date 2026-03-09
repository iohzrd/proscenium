-- Cached device announcements for peers (peer device discovery)
CREATE TABLE IF NOT EXISTS peer_device_announcements (
    master_pubkey TEXT PRIMARY KEY,
    announcement_json TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,
    cached_at INTEGER NOT NULL
);

-- Add transport_node_ids_json to peer_delegations (multiple devices per user)
-- The existing transport_node_id column only tracks a single device.
ALTER TABLE peer_delegations ADD COLUMN transport_node_ids_json TEXT NOT NULL DEFAULT '[]';
