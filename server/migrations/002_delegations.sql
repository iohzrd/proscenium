-- Add transport_node_id and delegation to registrations
ALTER TABLE registrations ADD COLUMN transport_node_id TEXT;
ALTER TABLE registrations ADD COLUMN delegation_json TEXT;

-- Cached delegations for peers (used for signature verification in ingestion)
CREATE TABLE IF NOT EXISTS peer_delegations (
    master_pubkey TEXT PRIMARY KEY,
    user_pubkey TEXT NOT NULL,
    delegation_json TEXT NOT NULL,
    transport_node_id TEXT,
    cached_at INTEGER NOT NULL
);
