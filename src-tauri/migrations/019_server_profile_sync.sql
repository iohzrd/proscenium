-- `updated_at` of the last own profile a server accepted. A registered server with an
-- older value receives the current profile again.
ALTER TABLE servers ADD COLUMN profile_synced_at INTEGER NOT NULL DEFAULT 0;
