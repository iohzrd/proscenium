-- Bookmarks become last-writer-wins rows, merged between a user's devices like follows
-- and moderation entries. A removed bookmark stays as a row with state 'removed'.
ALTER TABLE bookmarks ADD COLUMN state TEXT NOT NULL DEFAULT 'active';
ALTER TABLE bookmarks ADD COLUMN last_changed_at INTEGER NOT NULL DEFAULT 0;
UPDATE bookmarks SET last_changed_at = created_at;
