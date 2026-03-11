use iroh_social_types::{ConversationMeta, MediaAttachment, StoredMessage};
use rusqlite::params;
use sha2::{Digest, Sha256};

use super::Storage;

impl Storage {
    pub fn conversation_id(pubkey_a: &str, pubkey_b: &str) -> String {
        let mut keys = [pubkey_a, pubkey_b];
        keys.sort();
        let mut hasher = Sha256::new();
        hasher.update(b"iroh-social-dm-v1:");
        hasher.update(keys[0].as_bytes());
        hasher.update(b":");
        hasher.update(keys[1].as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub fn upsert_conversation(
        &self,
        peer_pubkey: &str,
        my_pubkey: &str,
        last_message_at: u64,
        preview: &str,
    ) -> anyhow::Result<()> {
        let conv_id = Self::conversation_id(my_pubkey, peer_pubkey);
        self.with_db(|db| {
            db.execute(
                "INSERT INTO dm_conversations (conversation_id, peer_pubkey, last_message_at, last_message_preview, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?3)
                 ON CONFLICT(conversation_id) DO UPDATE SET last_message_at=?3, last_message_preview=?4",
                params![conv_id, peer_pubkey, last_message_at as i64, preview],
            )?;
            Ok(())
        })
    }

    pub fn get_conversations(&self) -> anyhow::Result<Vec<ConversationMeta>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT peer_pubkey, last_message_at, last_message_preview, unread_count
                 FROM dm_conversations ORDER BY last_message_at DESC",
            )?;
            let mut rows = stmt.query([])?;
            let mut convos = Vec::new();
            while let Some(row) = rows.next()? {
                convos.push(ConversationMeta {
                    peer_pubkey: row.get(0)?,
                    last_message_at: row.get::<_, i64>(1)? as u64,
                    last_message_preview: row.get(2)?,
                    unread_count: row.get::<_, i32>(3)? as u32,
                });
            }
            Ok(convos)
        })
    }

    pub fn mark_conversation_read(&self, peer_pubkey: &str, my_pubkey: &str) -> anyhow::Result<()> {
        let conv_id = Self::conversation_id(my_pubkey, peer_pubkey);
        self.with_db(|db| {
            db.execute(
                "UPDATE dm_conversations SET unread_count = 0 WHERE conversation_id=?1",
                params![conv_id],
            )?;
            db.execute(
                "UPDATE dm_messages SET read = 1 WHERE conversation_id=?1 AND read = 0",
                params![conv_id],
            )?;
            Ok(())
        })
    }

    pub fn insert_dm_message(&self, msg: &StoredMessage) -> anyhow::Result<()> {
        let media_json = serde_json::to_string(&msg.media)?;
        self.with_db(|db| {
            db.execute(
                "INSERT OR IGNORE INTO dm_messages (id, conversation_id, from_pubkey, to_pubkey, content, timestamp, media_json, read, delivered, reply_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    msg.id,
                    msg.conversation_id,
                    msg.from_pubkey,
                    msg.to_pubkey,
                    msg.content,
                    msg.timestamp as i64,
                    media_json,
                    msg.read as i32,
                    msg.delivered as i32,
                    msg.reply_to,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_dm_messages(
        &self,
        conversation_id: &str,
        limit: usize,
        before: Option<u64>,
    ) -> anyhow::Result<Vec<StoredMessage>> {
        self.with_db(|db| {
            let mut messages = Vec::new();
            match before {
                Some(b) => {
                    let mut stmt = db.prepare(
                        "SELECT id, conversation_id, from_pubkey, to_pubkey, content, timestamp, media_json, read, delivered, reply_to
                         FROM dm_messages WHERE conversation_id=?1 AND timestamp < ?2
                         ORDER BY timestamp DESC LIMIT ?3",
                    )?;
                    let mut rows =
                        stmt.query(params![conversation_id, b as i64, limit as i64])?;
                    while let Some(row) = rows.next()? {
                        messages.push(Self::row_to_stored_message(row)?);
                    }
                }
                None => {
                    let mut stmt = db.prepare(
                        "SELECT id, conversation_id, from_pubkey, to_pubkey, content, timestamp, media_json, read, delivered, reply_to
                         FROM dm_messages WHERE conversation_id=?1
                         ORDER BY timestamp DESC LIMIT ?2",
                    )?;
                    let mut rows = stmt.query(params![conversation_id, limit as i64])?;
                    while let Some(row) = rows.next()? {
                        messages.push(Self::row_to_stored_message(row)?);
                    }
                }
            }
            messages.reverse();
            Ok(messages)
        })
    }

    fn row_to_stored_message(row: &rusqlite::Row) -> anyhow::Result<StoredMessage> {
        let media_json: String = row.get(6)?;
        let media: Vec<MediaAttachment> = serde_json::from_str(&media_json)?;
        Ok(StoredMessage {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            from_pubkey: row.get(2)?,
            to_pubkey: row.get(3)?,
            content: row.get(4)?,
            timestamp: row.get::<_, i64>(5)? as u64,
            media,
            read: row.get::<_, i32>(7)? != 0,
            delivered: row.get::<_, i32>(8)? != 0,
            reply_to: row.get(9)?,
        })
    }

    pub fn mark_dm_delivered(&self, message_id: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "UPDATE dm_messages SET delivered = 1 WHERE id=?1",
                params![message_id],
            )?;
            Ok(())
        })
    }

    pub fn mark_dm_read_by_id(&self, message_id: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "UPDATE dm_messages SET read = 1 WHERE id=?1",
                params![message_id],
            )?;
            Ok(())
        })
    }

    pub fn delete_dm_message(&self, message_id: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let count = db.execute("DELETE FROM dm_messages WHERE id=?1", params![message_id])?;
            Ok(count > 0)
        })
    }

    pub fn get_total_unread_count(&self) -> anyhow::Result<u32> {
        self.with_db(|db| {
            let count: i64 = db.query_row(
                "SELECT COALESCE(SUM(unread_count), 0) FROM dm_conversations",
                [],
                |row| row.get(0),
            )?;
            Ok(count as u32)
        })
    }

    pub fn insert_outbox_message(
        &self,
        id: &str,
        peer_pubkey: &str,
        envelope_json: &str,
        created_at: u64,
        message_id: &str,
    ) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "INSERT INTO dm_outbox (id, peer_pubkey, envelope_json, created_at, message_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id,
                    peer_pubkey,
                    envelope_json,
                    created_at as i64,
                    message_id
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_outbox_for_peer(
        &self,
        peer_pubkey: &str,
    ) -> anyhow::Result<Vec<(String, String, String)>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT id, envelope_json, message_id FROM dm_outbox WHERE peer_pubkey=?1 ORDER BY created_at ASC",
            )?;
            let mut rows = stmt.query(params![peer_pubkey])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                entries.push((row.get(0)?, row.get(1)?, row.get(2)?));
            }
            Ok(entries)
        })
    }

    pub fn get_all_outbox_peers(&self) -> anyhow::Result<Vec<String>> {
        self.with_db(|db| {
            let mut stmt = db.prepare("SELECT DISTINCT peer_pubkey FROM dm_outbox")?;
            let mut rows = stmt.query([])?;
            let mut peers = Vec::new();
            while let Some(row) = rows.next()? {
                peers.push(row.get(0)?);
            }
            Ok(peers)
        })
    }

    pub fn remove_outbox_message(&self, id: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute("DELETE FROM dm_outbox WHERE id=?1", params![id])?;
            Ok(())
        })
    }

    /// Atomically save ratchet state + conversation upsert + message insert + unread increment
    /// in a single SQLite transaction. `peer_signing_pubkey` keys the ratchet session;
    /// `peer_master_pubkey` is the conversation identity stored in `dm_conversations.peer_pubkey`.
    pub fn receive_dm_message_atomically(
        &self,
        peer_signing_pubkey: &str,
        peer_master_pubkey: &str,
        ratchet_state: &str,
        ratchet_updated_at: u64,
        message: &StoredMessage,
        preview: &str,
    ) -> anyhow::Result<()> {
        let conv_id = &message.conversation_id;
        let media_json = serde_json::to_string(&message.media)?;
        self.with_db_mut(|db| {
            let tx = db.transaction()?;

            tx.execute(
                "INSERT INTO dm_ratchet_sessions (peer_pubkey, state_json, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(peer_pubkey) DO UPDATE SET state_json=?2, updated_at=?3",
                params![peer_signing_pubkey, ratchet_state, ratchet_updated_at as i64],
            )?;

            tx.execute(
                "INSERT INTO dm_conversations
                     (conversation_id, peer_pubkey, last_message_at, last_message_preview, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?3)
                     ON CONFLICT(conversation_id)
                     DO UPDATE SET last_message_at=?3, last_message_preview=?4",
                params![conv_id, peer_master_pubkey, message.timestamp as i64, preview],
            )?;

            tx.execute(
                "INSERT OR IGNORE INTO dm_messages
                     (id, conversation_id, from_pubkey, to_pubkey, content, timestamp,
                      media_json, read, delivered, reply_to)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    message.id,
                    conv_id,
                    message.from_pubkey,
                    message.to_pubkey,
                    message.content,
                    message.timestamp as i64,
                    media_json,
                    message.read as i32,
                    message.delivered as i32,
                    message.reply_to,
                ],
            )?;

            tx.execute(
                "UPDATE dm_conversations SET unread_count = unread_count + 1
                 WHERE conversation_id=?1",
                params![conv_id],
            )?;

            tx.commit()?;
            Ok(())
        })
    }
}
