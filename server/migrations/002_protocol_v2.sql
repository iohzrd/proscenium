-- Protocol version 2: signed bytes carry a `proscenium/<type>/2` tag and post ids are
-- derived from post content. Rows stored under version 1 cannot be verified or matched
-- by id, so every table is dropped and created again.

DROP TRIGGER IF EXISTS posts_ai;
DROP TRIGGER IF EXISTS posts_ad;
DROP TABLE IF EXISTS posts_fts;
DROP TABLE IF EXISTS interactions;
DROP TABLE IF EXISTS posts;
DROP TABLE IF EXISTS sync_state;
DROP TABLE IF EXISTS trending_hashtags;
DROP TABLE IF EXISTS peer_device_announcements;
DROP TABLE IF EXISTS peer_delegations;
DROP TABLE IF EXISTS registrations;

-- One row per master pubkey that ever registered. A row is never deleted:
-- unregistration sets is_active = 0 and clears every column except the pubkey, the
-- timestamps and the five replay/downgrade floors at the end of the table.
CREATE TABLE registrations (
    pubkey TEXT PRIMARY KEY,
    registered_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    -- Visibility named in the registration request: 'public' or 'listed'.
    requested_visibility TEXT NOT NULL,
    -- Visibility of the newest stored profile: 'public' or 'listed'; NULL if none is stored.
    profile_visibility TEXT,
    -- 'public' if requested_visibility is 'public' and profile_visibility is not 'listed',
    -- else 'listed'. Posts are stored and served only while this is 'public'.
    visibility TEXT NOT NULL,
    display_name TEXT,
    bio TEXT,
    avatar_hash TEXT,
    -- NodeId of the device that sent the registration request.
    transport_node_id TEXT,
    -- Key that signs the user's content, from the newest accepted delegation.
    signing_pubkey TEXT,
    delegation_json TEXT,
    -- JSON array of the NodeIds in the newest accepted device announcement.
    device_node_ids_json TEXT,
    -- Greatest `timestamp` of an accepted signed HTTP request.
    last_request_timestamp INTEGER NOT NULL DEFAULT 0,
    -- Indexes of the newest accepted delegation.
    key_index INTEGER NOT NULL DEFAULT 0,
    dm_key_index INTEGER NOT NULL DEFAULT 0,
    -- `updated_at` of the newest accepted profile.
    profile_updated_at INTEGER NOT NULL DEFAULT 0,
    -- `version` of the newest accepted device announcement.
    announcement_version INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_registrations_active ON registrations(is_active, last_seen DESC);

-- `seq` is the rowid referenced by posts_fts. An explicit INTEGER PRIMARY KEY keeps its
-- value across VACUUM.
CREATE TABLE posts (
    seq INTEGER PRIMARY KEY,
    id TEXT NOT NULL,
    author TEXT NOT NULL REFERENCES registrations(pubkey),
    content TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    media_json TEXT,
    reply_to TEXT,
    reply_to_author TEXT,
    quote_of TEXT,
    quote_of_author TEXT,
    signature TEXT NOT NULL,
    indexed_at INTEGER NOT NULL,
    UNIQUE (author, id)
);

CREATE INDEX idx_posts_timestamp ON posts(timestamp DESC, id DESC);
CREATE INDEX idx_posts_author_timestamp ON posts(author, timestamp DESC, id DESC);
CREATE INDEX idx_posts_reply_to ON posts(reply_to_author, reply_to);
CREATE INDEX idx_posts_quote_of ON posts(quote_of_author, quote_of);

-- Ids of posts their author deleted. A post whose (author, id) is listed is not stored again.
CREATE TABLE post_tombstones (
    author TEXT NOT NULL REFERENCES registrations(pubkey),
    id TEXT NOT NULL,
    deleted_at INTEGER NOT NULL,
    PRIMARY KEY (author, id)
) WITHOUT ROWID;

CREATE TABLE interactions (
    id TEXT NOT NULL,
    author TEXT NOT NULL REFERENCES registrations(pubkey),
    kind TEXT NOT NULL,
    target_post_id TEXT NOT NULL,
    target_author TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    signature TEXT NOT NULL,
    indexed_at INTEGER NOT NULL,
    PRIMARY KEY (author, id)
);

CREATE INDEX idx_interactions_target ON interactions(target_author, target_post_id, kind, author);
CREATE INDEX idx_interactions_author ON interactions(author, timestamp DESC);
CREATE INDEX idx_interactions_timestamp ON interactions(timestamp);

CREATE VIRTUAL TABLE posts_fts USING fts5(
    content,
    content=posts,
    content_rowid=seq,
    tokenize='unicode61'
);

CREATE TRIGGER posts_ai AFTER INSERT ON posts BEGIN
    INSERT INTO posts_fts(rowid, content) VALUES (new.seq, new.content);
END;

CREATE TRIGGER posts_ad AFTER DELETE ON posts BEGIN
    INSERT INTO posts_fts(posts_fts, rowid, content) VALUES('delete', old.seq, old.content);
END;

CREATE TABLE trending_hashtags (
    tag TEXT PRIMARY KEY,
    post_count INTEGER NOT NULL,
    unique_authors INTEGER NOT NULL,
    latest_post_at INTEGER NOT NULL,
    score REAL NOT NULL,
    computed_at INTEGER NOT NULL
);
