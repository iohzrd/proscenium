CREATE TABLE IF NOT EXISTS follow_requests (
    pubkey TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
