-- Lookup of a post by id without its author (thread view, quote embeds).
CREATE INDEX IF NOT EXISTS idx_posts_id ON posts(id);

-- Keyset pagination orders by (timestamp, id). These replace the timestamp-only indexes.
CREATE INDEX IF NOT EXISTS idx_posts_timestamp_id ON posts(timestamp DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_posts_author_timestamp_id ON posts(author, timestamp DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_posts_reply_to_timestamp_id ON posts(reply_to, timestamp, id);
CREATE INDEX IF NOT EXISTS idx_interactions_author_timestamp_id ON interactions(author, timestamp, id);
CREATE INDEX IF NOT EXISTS idx_notifications_timestamp_id ON notifications(timestamp DESC, id DESC);
DROP INDEX IF EXISTS idx_posts_timestamp;
DROP INDEX IF EXISTS idx_posts_author_timestamp;
DROP INDEX IF EXISTS idx_posts_reply_to;
DROP INDEX IF EXISTS idx_interactions_author_timestamp;
DROP INDEX IF EXISTS idx_notifications_ts;

-- Deleting a post deletes its notifications by either column.
CREATE INDEX IF NOT EXISTS idx_notifications_post_id ON notifications(post_id);
CREATE INDEX IF NOT EXISTS idx_notifications_target_post_id ON notifications(target_post_id);

-- No code reads or writes these.
DROP TABLE IF EXISTS push_outbox;
ALTER TABLE servers DROP COLUMN last_synced_at;

-- `applied_at` was written in seconds; every other timestamp is in milliseconds.
UPDATE schema_migrations SET applied_at = applied_at * 1000 WHERE applied_at < 100000000000;
