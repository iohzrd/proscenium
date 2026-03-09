-- Cached signing key delegations and transport NodeIds for peers.
-- Populated by IdentityRequest responses, gossip announcements, and server lookups.
CREATE TABLE IF NOT EXISTS peer_delegations (
    master_pubkey TEXT PRIMARY KEY,
    signing_pubkey TEXT NOT NULL,
    delegation_json TEXT NOT NULL,
    transport_node_ids_json TEXT NOT NULL DEFAULT '[]',
    cached_at INTEGER NOT NULL
);
