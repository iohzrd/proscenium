CREATE TABLE IF NOT EXISTS follows (
    pubkey TEXT PRIMARY KEY,
    alias TEXT,
    followed_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    last_changed_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS followers (
    pubkey TEXT PRIMARY KEY,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    is_online INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS follow_requests (
    pubkey TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS mutes (
    pubkey TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    last_changed_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS blocks (
    pubkey TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    last_changed_at INTEGER NOT NULL DEFAULT 0
);
