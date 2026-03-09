CREATE TABLE IF NOT EXISTS posts (
    id TEXT NOT NULL,
    author TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    media_json TEXT NOT NULL DEFAULT '[]',
    reply_to TEXT,
    reply_to_author TEXT,
    quote_of TEXT,
    quote_of_author TEXT,
    signature TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (author, id)
);
CREATE INDEX IF NOT EXISTS idx_posts_timestamp ON posts(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_posts_author_timestamp ON posts(author, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_posts_reply_to ON posts(reply_to);
CREATE INDEX IF NOT EXISTS idx_posts_quote_of ON posts(quote_of);

CREATE TABLE IF NOT EXISTS interactions (
    id TEXT NOT NULL,
    author TEXT NOT NULL,
    kind TEXT NOT NULL,
    target_post_id TEXT NOT NULL,
    target_author TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    signature TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (author, id)
);
CREATE INDEX IF NOT EXISTS idx_interactions_target ON interactions(target_post_id, kind);
CREATE INDEX IF NOT EXISTS idx_interactions_author_timestamp ON interactions(author, timestamp DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_interactions_unique ON interactions(author, kind, target_post_id);
