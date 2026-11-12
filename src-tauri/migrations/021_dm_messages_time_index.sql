-- Device sync reads the newest message ids over all conversations.
CREATE INDEX IF NOT EXISTS idx_dm_messages_time ON dm_messages(timestamp DESC, id DESC);
