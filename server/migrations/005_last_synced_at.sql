-- Time in milliseconds of the last sync that completed with a device of the user; 0 if
-- none completed during the current registration. Syncs of a user with 0 wait for the
-- smaller pool of sync permits.
ALTER TABLE registrations ADD COLUMN last_synced_at INTEGER NOT NULL DEFAULT 0;
