CREATE TABLE IF NOT EXISTS notifications (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    actor TEXT NOT NULL,
    target_post_id TEXT,
    post_id TEXT,
    timestamp INTEGER NOT NULL,
    read INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_notifications_ts ON notifications(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_notifications_unread ON notifications(read, timestamp DESC);
