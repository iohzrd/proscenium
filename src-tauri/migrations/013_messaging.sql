-- dm_outbox holds one row per undelivered outgoing message. payload_json is the plaintext
-- DmPayload; the envelope is encrypted when the message is sent.
DROP TABLE IF EXISTS dm_outbox;
CREATE TABLE dm_outbox (
    message_id TEXT PRIMARY KEY,
    peer_pubkey TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_dm_outbox_due ON dm_outbox(next_attempt_at);
CREATE INDEX IF NOT EXISTS idx_dm_outbox_peer ON dm_outbox(peer_pubkey);

-- Existing sessions have a root key seeded with the Noise handshake hash, which is
-- computable from the handshake messages. Without a session row the next send runs a
-- new handshake.
DELETE FROM dm_ratchet_sessions;

-- get_dm_messages orders by (timestamp DESC, id DESC).
DROP INDEX IF EXISTS idx_dm_messages_conv_time;
CREATE INDEX IF NOT EXISTS idx_dm_messages_conv_time_id
    ON dm_messages(conversation_id, timestamp DESC, id DESC);
