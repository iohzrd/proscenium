-- The master key signs all content; signing key delegations are not stored. DMs are
-- encrypted to each device without session state.
DROP TABLE IF EXISTS peer_delegations;
DROP TABLE IF EXISTS dm_ratchet_sessions;
