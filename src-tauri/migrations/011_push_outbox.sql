CREATE TABLE IF NOT EXISTS push_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recipient TEXT NOT NULL,
    post_id TEXT,
    interaction_id TEXT,
    created_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 100,
    last_attempt_at INTEGER,
    expires_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_push_outbox_recipient ON push_outbox(recipient);
