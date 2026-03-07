ALTER TABLE profiles ADD COLUMN visibility TEXT NOT NULL DEFAULT 'public';
UPDATE profiles SET visibility = CASE WHEN is_private = 1 THEN 'private' ELSE 'public' END;
