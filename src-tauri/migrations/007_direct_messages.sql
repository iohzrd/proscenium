CREATE TABLE IF NOT EXISTS dm_conversations (
    conversation_id TEXT PRIMARY KEY,
    peer_pubkey TEXT NOT NULL UNIQUE,
    last_message_at INTEGER NOT NULL DEFAULT 0,
    last_message_preview TEXT NOT NULL DEFAULT '',
    unread_count INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_dm_conversations_last_msg ON dm_conversations(last_message_at DESC);

CREATE TABLE IF NOT EXISTS dm_messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    from_pubkey TEXT NOT NULL,
    to_pubkey TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    media_json TEXT NOT NULL DEFAULT '[]',
    read INTEGER NOT NULL DEFAULT 0,
    delivered INTEGER NOT NULL DEFAULT 0,
    reply_to TEXT,
    FOREIGN KEY (conversation_id) REFERENCES dm_conversations(conversation_id)
);
CREATE INDEX IF NOT EXISTS idx_dm_messages_conv_time ON dm_messages(conversation_id, timestamp DESC);

CREATE TABLE IF NOT EXISTS dm_outbox (
    id TEXT PRIMARY KEY,
    peer_pubkey TEXT NOT NULL,
    envelope_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_retry_at INTEGER,
    message_id TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_dm_outbox_peer ON dm_outbox(peer_pubkey, created_at ASC);

CREATE TABLE IF NOT EXISTS dm_ratchet_sessions (
    peer_pubkey TEXT PRIMARY KEY,
    state_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
