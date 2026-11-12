-- Time a device of the profile's owner last sent it with Public visibility in a sync
-- summary. A node serves another author's posts as a mirror only while this is recent.
ALTER TABLE profiles ADD COLUMN public_confirmed_at INTEGER NOT NULL DEFAULT 0;
