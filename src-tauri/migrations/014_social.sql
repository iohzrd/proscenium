-- Follow and follower lists fetched from other users. `owner` is the user whose list the
-- row belongs to; `kind` is 'follows' or 'followers'. These rows are display data only and
-- are never read by an authorization check.
CREATE TABLE IF NOT EXISTS remote_social_graph (
    owner TEXT NOT NULL,
    kind TEXT NOT NULL,
    pubkey TEXT NOT NULL,
    followed_at INTEGER NOT NULL DEFAULT 0,
    first_seen INTEGER NOT NULL DEFAULT 0,
    last_seen INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (owner, kind, pubkey)
);

-- social_graph keeps only local facts: rows written by a local follow or unfollow
-- (last_changed_at > 0) and follower rows written for a verified follower (last_seen > 0).
-- Rows that came from another user's list response have neither and are deleted.
DELETE FROM social_graph
WHERE last_changed_at = 0 AND COALESCE(last_seen, 0) = 0;

-- Columns that a list response set on a remaining row are cleared.
UPDATE social_graph SET first_seen = NULL, last_seen = NULL, is_online = 0
WHERE COALESCE(last_seen, 0) = 0;
UPDATE social_graph SET followed_at = NULL
WHERE last_changed_at = 0;

-- The cached lists are gone, so their fetch times are removed and the lists are fetched again.
DELETE FROM remote_social_meta;

CREATE INDEX IF NOT EXISTS idx_social_graph_followee ON social_graph(followee);

-- Transport NodeId -> master pubkey. One row per NodeId; the row is written from the
-- device list of the most recently stored announcement that contains the NodeId.
CREATE TABLE IF NOT EXISTS peer_transports (
    node_id TEXT PRIMARY KEY,
    master_pubkey TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_peer_transports_master ON peer_transports(master_pubkey);

-- Copy the existing mappings. If two rows contain one NodeId, the row cached last is kept.
INSERT OR IGNORE INTO peer_transports (node_id, master_pubkey)
SELECT j.value, p.master_pubkey
FROM peer_delegations p,
     json_each(CASE WHEN json_valid(p.transport_node_ids_json)
                    THEN p.transport_node_ids_json ELSE '[]' END) j
WHERE j.type = 'text'
ORDER BY p.cached_at DESC, p.master_pubkey ASC;

CREATE INDEX IF NOT EXISTS idx_peer_delegations_dm_pubkey ON peer_delegations(dm_pubkey);

-- Signed deletions of posts. A post whose (author, id) is listed here is not stored again.
-- `signature` is the output of sign_delete_post; the row is sent to followers by push and sync.
CREATE TABLE IF NOT EXISTS post_tombstones (
    author TEXT NOT NULL,
    id TEXT NOT NULL,
    signature TEXT NOT NULL,
    deleted_at INTEGER NOT NULL,
    PRIMARY KEY (author, id)
);
CREATE INDEX IF NOT EXISTS idx_post_tombstones_author_deleted
    ON post_tombstones(author, deleted_at DESC);

-- Verified deletions of interactions. An interaction whose (author, id) is listed here is
-- not stored again.
CREATE TABLE IF NOT EXISTS interaction_tombstones (
    author TEXT NOT NULL,
    id TEXT NOT NULL,
    deleted_at INTEGER NOT NULL,
    PRIMARY KEY (author, id)
);
CREATE INDEX IF NOT EXISTS idx_interaction_tombstones_author_deleted
    ON interaction_tombstones(author, deleted_at DESC);
