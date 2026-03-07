CREATE TABLE IF NOT EXISTS registrations (
    pubkey TEXT PRIMARY KEY,
    registered_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    display_name TEXT,
    bio TEXT,
    avatar_hash TEXT,
    visibility TEXT NOT NULL DEFAULT 'public',
    is_active INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS posts (
    id TEXT NOT NULL,
    author TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    media_json TEXT,
    reply_to TEXT,
    reply_to_author TEXT,
    quote_of TEXT,
    quote_of_author TEXT,
    signature TEXT NOT NULL DEFAULT '',
    indexed_at INTEGER NOT NULL,
    PRIMARY KEY (author, id),
    FOREIGN KEY (author) REFERENCES registrations(pubkey)
);

CREATE INDEX IF NOT EXISTS idx_posts_timestamp ON posts(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_posts_author_timestamp ON posts(author, timestamp DESC);

CREATE TABLE IF NOT EXISTS interactions (
    id TEXT NOT NULL,
    author TEXT NOT NULL,
    kind TEXT NOT NULL,
    target_post_id TEXT NOT NULL,
    target_author TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    signature TEXT NOT NULL DEFAULT '',
    indexed_at INTEGER NOT NULL,
    PRIMARY KEY (author, id),
    FOREIGN KEY (author) REFERENCES registrations(pubkey)
);

CREATE INDEX IF NOT EXISTS idx_interactions_target ON interactions(target_author, target_post_id);
CREATE INDEX IF NOT EXISTS idx_interactions_author ON interactions(author, timestamp DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS posts_fts USING fts5(
    content,
    content=posts,
    content_rowid=rowid,
    tokenize='unicode61'
);

CREATE TRIGGER IF NOT EXISTS posts_ai AFTER INSERT ON posts BEGIN
    INSERT INTO posts_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS posts_ad AFTER DELETE ON posts BEGIN
    INSERT INTO posts_fts(posts_fts, rowid, content) VALUES('delete', old.rowid, old.content);
END;

CREATE TABLE IF NOT EXISTS trending_hashtags (
    tag TEXT PRIMARY KEY,
    post_count INTEGER NOT NULL,
    unique_authors INTEGER NOT NULL,
    latest_post_at INTEGER NOT NULL,
    score REAL NOT NULL,
    computed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_state (
    pubkey TEXT PRIMARY KEY,
    last_synced_at INTEGER NOT NULL,
    last_post_timestamp INTEGER,
    last_interaction_timestamp INTEGER,
    FOREIGN KEY (pubkey) REFERENCES registrations(pubkey)
);
