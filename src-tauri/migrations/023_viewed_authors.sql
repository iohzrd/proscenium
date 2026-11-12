-- Authors the local user does not follow whose content a profile view synced, and when.
CREATE TABLE IF NOT EXISTS viewed_authors (
    pubkey TEXT PRIMARY KEY NOT NULL,
    synced_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_viewed_authors_time ON viewed_authors(synced_at DESC);
