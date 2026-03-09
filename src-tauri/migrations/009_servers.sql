CREATE TABLE IF NOT EXISTS servers (
    url TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    node_id TEXT NOT NULL DEFAULT '',
    registered_at INTEGER,
    visibility TEXT NOT NULL DEFAULT 'public',
    added_at INTEGER NOT NULL,
    last_synced_at INTEGER
);
