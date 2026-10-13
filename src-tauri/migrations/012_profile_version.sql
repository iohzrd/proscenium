-- Signed profile version (Unix ms). A stored profile is replaced only by an equal or newer one.
ALTER TABLE profiles ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
