-- Unified social graph: each row is a directional relationship.
-- "follower follows followee"
-- Your follows: WHERE follower = your_pubkey AND state = 'active'
-- Your followers: WHERE followee = your_pubkey
-- Remote cache: same queries with a different pubkey
CREATE TABLE IF NOT EXISTS social_graph (
    follower TEXT NOT NULL,
    followee TEXT NOT NULL,
    followed_at INTEGER,
    state TEXT NOT NULL DEFAULT 'active',
    last_changed_at INTEGER NOT NULL DEFAULT 0,
    first_seen INTEGER,
    last_seen INTEGER,
    is_online INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (follower, followee)
);

CREATE TABLE IF NOT EXISTS follow_requests (
    pubkey TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS moderation (
    pubkey TEXT NOT NULL,
    kind TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    last_changed_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (pubkey, kind)
);

-- Track cache freshness and visibility for remote users' social lists.
CREATE TABLE IF NOT EXISTS remote_social_meta (
    pubkey TEXT PRIMARY KEY,
    follows_hidden INTEGER NOT NULL DEFAULT 0,
    followers_hidden INTEGER NOT NULL DEFAULT 0,
    follows_fetched_at INTEGER,
    followers_fetched_at INTEGER
);
