-- The master key signs all content; signing key delegations are not stored. The DM pubkey
-- comes from the signed device list. Cached announcements written before this change have
-- no DM pubkey and keep their device lists until a newer announcement replaces them.
DROP TABLE IF EXISTS peer_delegations;
ALTER TABLE peer_device_announcements ADD COLUMN dm_pubkey TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_peer_device_announcements_dm_pubkey
    ON peer_device_announcements(dm_pubkey);

-- DM keys are derived from the master key without an index now, so every stored session
-- belongs to a DM key pair that is no longer used.
DELETE FROM dm_ratchet_sessions;
