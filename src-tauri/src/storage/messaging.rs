use crate::error::AppError;
use proscenium_types::{ConversationMeta, MediaAttachment, StoredMessage};
use sha2::{Digest, Sha256};
use sqlx::Row;

use super::Storage;

/// Conversations with strangers that are kept: peers the local user does not follow and
/// has not written to in that conversation. A new one evicts the one with the oldest
/// last message.
pub const MAX_STRANGER_CONVERSATIONS: i64 = 100;
/// Messages of a stranger stored in its conversation before further ones are refused.
pub const MAX_STRANGER_MESSAGES: i64 = 50;

/// Condition on a conversation `c` and the local user `?1`: the peer is not followed and
/// the local user wrote no message in it.
const STRANGER_CONVERSATION: &str = "NOT EXISTS (SELECT 1 FROM social_graph s
         WHERE s.follower = ?1 AND s.followee = c.peer_pubkey
           AND s.state = 'active' AND s.followed_at IS NOT NULL)
     AND NOT EXISTS (SELECT 1 FROM dm_messages m
         WHERE m.conversation_id = c.conversation_id AND m.from_pubkey = ?1)";

/// Result of `Storage::receive_dm_message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceivedDm {
    /// The message was stored.
    Stored,
    /// A message with the same id is stored; nothing was written.
    Duplicate,
    /// The sender is a stranger with `MAX_STRANGER_MESSAGES` messages in the
    /// conversation; nothing was written.
    Refused,
}

/// Convert a millisecond timestamp to the SQLite integer type; values above `i64::MAX`
/// become `i64::MAX`.
fn to_db_millis(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Storage {
    pub fn conversation_id(pubkey_a: &str, pubkey_b: &str) -> String {
        let mut keys = [pubkey_a, pubkey_b];
        keys.sort();
        let mut hasher = Sha256::new();
        hasher.update(b"proscenium-dm-v1:");
        hasher.update(keys[0].as_bytes());
        hasher.update(b":");
        hasher.update(keys[1].as_bytes());
        hex::encode(hasher.finalize())
    }

    pub async fn get_conversations(&self) -> Result<Vec<ConversationMeta>, AppError> {
        let rows = sqlx::query(
            "SELECT peer_pubkey, last_message_at, last_message_preview, unread_count
             FROM dm_conversations ORDER BY last_message_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut convos = Vec::new();
        for row in &rows {
            convos.push(ConversationMeta {
                peer_pubkey: row.get(0),
                last_message_at: row.get::<i64, _>(1) as u64,
                last_message_preview: row.get(2),
                unread_count: row.get::<i32, _>(3) as u32,
            });
        }
        Ok(convos)
    }

    /// Set the unread count of the conversation with `peer_pubkey` to 0 and mark the
    /// messages received from `peer_pubkey` as read. The local user's own messages keep
    /// their `read` flag, which the peer's read receipt sets.
    pub async fn mark_conversation_read(
        &self,
        peer_pubkey: &str,
        my_pubkey: &str,
    ) -> Result<(), AppError> {
        let conv_id = Self::conversation_id(my_pubkey, peer_pubkey);
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE dm_conversations SET unread_count = 0 WHERE conversation_id=?1")
            .bind(&conv_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE dm_messages SET read = 1
             WHERE conversation_id=?1 AND from_pubkey=?2 AND read = 0",
        )
        .bind(&conv_id)
        .bind(peer_pubkey)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Store a message written by the local user: the conversation row (created or its
    /// preview replaced), the message row, and the outbox entry holding `payload_json`,
    /// in one transaction. The outbox entry is not selected by
    /// `get_due_outbox_entries` before `next_attempt_at`.
    pub async fn insert_outgoing_dm(
        &self,
        message: &StoredMessage,
        preview: &str,
        payload_json: &str,
        next_attempt_at: u64,
    ) -> Result<(), AppError> {
        let media_json = serde_json::to_string(&message.media)?;
        let timestamp = to_db_millis(message.timestamp);

        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO dm_conversations
                 (conversation_id, peer_pubkey, last_message_at, last_message_preview, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?3)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                    last_message_at = MAX(last_message_at, ?3),
                    last_message_preview = ?4",
        )
        .bind(&message.conversation_id)
        .bind(&message.to_pubkey)
        .bind(timestamp)
        .bind(preview)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO dm_messages
                 (id, conversation_id, from_pubkey, to_pubkey, content, timestamp,
                  media_json, read, delivered, reply_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(&message.id)
        .bind(&message.conversation_id)
        .bind(&message.from_pubkey)
        .bind(&message.to_pubkey)
        .bind(&message.content)
        .bind(timestamp)
        .bind(&media_json)
        .bind(message.read as i32)
        .bind(message.delivered as i32)
        .bind(&message.reply_to)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO dm_outbox
                 (message_id, peer_pubkey, payload_json, created_at, attempts, next_attempt_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5)",
        )
        .bind(&message.id)
        .bind(&message.to_pubkey)
        .bind(payload_json)
        .bind(timestamp)
        .bind(to_db_millis(next_attempt_at))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Return up to `limit` messages of a conversation in ascending `(timestamp, id)`
    /// order: the newest ones, or with a cursor the newest ones older than the cursor.
    ///
    /// `before` and `before_id` are the timestamp and id of the oldest message of the
    /// previous page. With `before` alone, only messages with a lower timestamp are returned.
    pub async fn get_dm_messages(
        &self,
        conversation_id: &str,
        limit: usize,
        before: Option<u64>,
        before_id: Option<&str>,
    ) -> Result<Vec<StoredMessage>, AppError> {
        let rows = sqlx::query(
            "SELECT id, conversation_id, from_pubkey, to_pubkey, content, timestamp, media_json, read, delivered, reply_to
             FROM dm_messages
             WHERE conversation_id=?1
               AND (?2 IS NULL OR timestamp < ?2 OR (timestamp = ?2 AND ?3 IS NOT NULL AND id < ?3))
             ORDER BY timestamp DESC, id DESC LIMIT ?4",
        )
        .bind(conversation_id)
        .bind(before.map(to_db_millis))
        .bind(before_id)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        let mut messages = Vec::with_capacity(rows.len());
        for row in &rows {
            messages.push(Self::row_to_stored_message(row)?);
        }
        messages.reverse();
        Ok(messages)
    }

    fn row_to_stored_message(row: &sqlx::sqlite::SqliteRow) -> Result<StoredMessage, AppError> {
        let media_json: String = row.get(6);
        let media: Vec<MediaAttachment> = serde_json::from_str(&media_json)?;
        Ok(StoredMessage {
            id: row.get(0),
            conversation_id: row.get(1),
            from_pubkey: row.get(2),
            to_pubkey: row.get(3),
            content: row.get(4),
            timestamp: row.get::<i64, _>(5) as u64,
            media,
            read: row.get::<i32, _>(7) != 0,
            delivered: row.get::<i32, _>(8) != 0,
            reply_to: row.get(9),
        })
    }

    /// Set `delivered` on message `message_id` and delete its outbox entry, only if the
    /// message is in `conversation_id` and was sent by `from_pubkey`.
    /// Returns true if such a message exists.
    pub async fn mark_dm_delivered(
        &self,
        conversation_id: &str,
        from_pubkey: &str,
        message_id: &str,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE dm_messages SET delivered = 1
             WHERE id=?1 AND conversation_id=?2 AND from_pubkey=?3",
        )
        .bind(message_id)
        .bind(conversation_id)
        .bind(from_pubkey)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if updated {
            sqlx::query("DELETE FROM dm_outbox WHERE message_id=?1")
                .bind(message_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(updated)
    }

    /// Set `read` on message `message_id` and on every earlier message, by
    /// `(timestamp, id)`, that `from_pubkey` sent in `conversation_id`: the meaning of a
    /// `DmPayload::Read` receipt. Nothing changes unless `message_id` is a message of
    /// `from_pubkey` in `conversation_id`. Returns true if `message_id` exists there.
    pub async fn mark_dm_read_up_to(
        &self,
        conversation_id: &str,
        from_pubkey: &str,
        message_id: &str,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let reference: Option<i64> = sqlx::query_scalar(
            "SELECT timestamp FROM dm_messages
             WHERE id=?1 AND conversation_id=?2 AND from_pubkey=?3",
        )
        .bind(message_id)
        .bind(conversation_id)
        .bind(from_pubkey)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(timestamp) = reference else {
            tx.commit().await?;
            return Ok(false);
        };
        sqlx::query(
            "UPDATE dm_messages SET read = 1
             WHERE conversation_id=?1 AND from_pubkey=?2 AND read = 0
               AND (timestamp < ?3 OR (timestamp = ?3 AND id <= ?4))",
        )
        .bind(conversation_id)
        .bind(from_pubkey)
        .bind(timestamp)
        .bind(message_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Delete a message and its outbox entry in one transaction.
    /// Returns true if the message row existed.
    pub async fn delete_dm_message(&self, message_id: &str) -> Result<bool, AppError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM dm_outbox WHERE message_id=?1")
            .bind(message_id)
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM dm_messages WHERE id=?1")
            .bind(message_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_total_unread_count(&self) -> Result<u32, AppError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(unread_count), 0) FROM dm_conversations")
                .fetch_one(&self.pool)
                .await?;
        Ok(count as u32)
    }

    /// Return `(message_id, peer_pubkey, attempts)` of up to `limit` outbox entries,
    /// oldest first: those with `next_attempt_at <= now`, and if `include_backed_off`
    /// also every entry with at least one failed attempt.
    pub async fn get_due_outbox_entries(
        &self,
        now: u64,
        include_backed_off: bool,
        limit: u32,
    ) -> Result<Vec<(String, String, u32)>, AppError> {
        let rows = sqlx::query(
            "SELECT message_id, peer_pubkey, attempts FROM dm_outbox
             WHERE next_attempt_at <= ?1 OR (?2 AND attempts > 0)
             ORDER BY created_at ASC, message_id ASC LIMIT ?3",
        )
        .bind(to_db_millis(now))
        .bind(include_backed_off)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let attempts = u32::try_from(r.get::<i64, _>(2)).unwrap_or(u32::MAX);
                (r.get(0), r.get(1), attempts)
            })
            .collect())
    }

    /// Return the queued `DmPayload` JSON of a message, or `None` if it is not queued.
    pub async fn get_outbox_payload(&self, message_id: &str) -> Result<Option<String>, AppError> {
        let payload: Option<String> =
            sqlx::query_scalar("SELECT payload_json FROM dm_outbox WHERE message_id=?1")
                .bind(message_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(payload)
    }

    pub async fn remove_outbox_message(&self, message_id: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM dm_outbox WHERE message_id=?1")
            .bind(message_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record a failed delivery of `message_id`: increment its attempt counter and set its
    /// next attempt time. Every other entry for `peer_pubkey` with an earlier next attempt
    /// time is moved to `next_attempt_at`; its attempt counter is unchanged.
    pub async fn record_outbox_failure(
        &self,
        message_id: &str,
        peer_pubkey: &str,
        next_attempt_at: u64,
    ) -> Result<(), AppError> {
        let next_attempt_at = to_db_millis(next_attempt_at);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE dm_outbox SET attempts = attempts + 1, next_attempt_at = ?2
             WHERE message_id=?1",
        )
        .bind(message_id)
        .bind(next_attempt_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE dm_outbox SET next_attempt_at = ?2
             WHERE peer_pubkey=?3 AND message_id != ?1 AND next_attempt_at < ?2",
        )
        .bind(message_id)
        .bind(next_attempt_at)
        .bind(peer_pubkey)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Clear the next attempt time of the entries for `peer_pubkey` that have at least one
    /// failed attempt and a next attempt time after `now`. Returns the number of entries
    /// changed. Entries without a failed attempt keep their time: their first delivery
    /// may still be running.
    pub async fn make_peer_outbox_due(&self, peer_pubkey: &str, now: u64) -> Result<u64, AppError> {
        let result = sqlx::query(
            "UPDATE dm_outbox SET next_attempt_at = 0
             WHERE peer_pubkey=?1 AND attempts > 0 AND next_attempt_at > ?2",
        )
        .bind(peer_pubkey)
        .bind(to_db_millis(now))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Delete outbox entries created before `created_before`. Returns the number deleted.
    pub async fn delete_expired_outbox(&self, created_before: u64) -> Result<u64, AppError> {
        let result = sqlx::query("DELETE FROM dm_outbox WHERE created_at < ?1")
            .bind(to_db_millis(created_before))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// True if `peer_pubkey` is a stranger to `me`: `me` does not follow it and wrote no
    /// message in their conversation.
    pub async fn is_stranger(&self, me: &str, peer_pubkey: &str) -> Result<bool, AppError> {
        let conv_id = Self::conversation_id(me, peer_pubkey);
        let stranger: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS (SELECT 1 FROM social_graph s
                     WHERE s.follower = ?1 AND s.followee = ?2
                       AND s.state = 'active' AND s.followed_at IS NOT NULL)
                AND NOT EXISTS (SELECT 1 FROM dm_messages m
                     WHERE m.conversation_id = ?3 AND m.from_pubkey = ?1)",
        )
        .bind(me)
        .bind(peer_pubkey)
        .bind(&conv_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(stranger)
    }

    /// True if a conversation with `peer_pubkey` is stored.
    pub async fn has_conversation(&self, peer_pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM dm_conversations WHERE peer_pubkey = ?1)",
        )
        .bind(peer_pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Store a message received from `peer_master_pubkey` and update its conversation:
    /// the preview and `last_message_at` follow the newest message, and `unread_count`
    /// grows by one.
    ///
    /// For a stranger (see `MAX_STRANGER_CONVERSATIONS`), at most `MAX_STRANGER_MESSAGES`
    /// messages are stored in its conversation, and a new stranger conversation beyond
    /// `MAX_STRANGER_CONVERSATIONS` evicts the stranger conversation with the oldest last
    /// message, with its messages and the cached identity of its peer if nothing else
    /// refers to it. Messages from generated identities therefore cannot fill the
    /// database.
    pub async fn receive_dm_message(
        &self,
        peer_master_pubkey: &str,
        message: &StoredMessage,
        preview: &str,
    ) -> Result<ReceivedDm, AppError> {
        self.insert_dm_message(peer_master_pubkey, message, preview, 1, true)
            .await
    }

    /// Store a message of the conversation with `peer_pubkey` that another device of the
    /// local user sent during device sync. `incoming` is true if `peer_pubkey` wrote it;
    /// `unread_count` grows by one only for an incoming message that is not read. An
    /// incoming message is subject to the stranger limits of `receive_dm_message`.
    /// Returns false, and writes nothing, if a message with the same id is stored or the
    /// limits refuse it.
    pub async fn import_synced_dm(
        &self,
        peer_pubkey: &str,
        message: &StoredMessage,
        incoming: bool,
    ) -> Result<bool, AppError> {
        let preview = crate::util::message_preview(&message.content);
        let unread = i64::from(incoming && !message.read);
        Ok(self
            .insert_dm_message(peer_pubkey, message, &preview, unread, incoming)
            .await?
            == ReceivedDm::Stored)
    }

    /// Insert `message` and create or update its conversation with `peer_pubkey`, in one
    /// transaction, adding `unread_increment` to `unread_count`. Writes nothing if a
    /// message with the same id is stored. With `limit_strangers`, applies the limits of
    /// `receive_dm_message` to a message from `peer_pubkey` to `message.to_pubkey`.
    async fn insert_dm_message(
        &self,
        peer_pubkey: &str,
        message: &StoredMessage,
        preview: &str,
        unread_increment: i64,
        limit_strangers: bool,
    ) -> Result<ReceivedDm, AppError> {
        let conv_id = &message.conversation_id;
        let media_json = serde_json::to_string(&message.media)?;
        let timestamp = to_db_millis(message.timestamp);

        // BEGIN IMMEDIATE takes the write lock, so no other writer can insert the same
        // message id between the existence check and the insert below.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        let exists: bool = sqlx::query_scalar("SELECT COUNT(*) > 0 FROM dm_messages WHERE id=?1")
            .bind(&message.id)
            .fetch_one(&mut *tx)
            .await?;
        if exists {
            tx.commit().await?;
            return Ok(ReceivedDm::Duplicate);
        }
        if limit_strangers
            && !self
                .admit_stranger_message_tx(&mut tx, peer_pubkey, message)
                .await?
        {
            tx.commit().await?;
            return Ok(ReceivedDm::Refused);
        }

        sqlx::query(
            "INSERT INTO dm_conversations
                 (conversation_id, peer_pubkey, last_message_at, last_message_preview,
                  unread_count, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?3)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                    unread_count = unread_count + ?5,
                    last_message_preview = CASE WHEN ?3 >= last_message_at
                        THEN ?4 ELSE last_message_preview END,
                    last_message_at = MAX(last_message_at, ?3)",
        )
        .bind(conv_id)
        .bind(peer_pubkey)
        .bind(timestamp)
        .bind(preview)
        .bind(unread_increment)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO dm_messages
                 (id, conversation_id, from_pubkey, to_pubkey, content, timestamp,
                  media_json, read, delivered, reply_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(&message.id)
        .bind(conv_id)
        .bind(&message.from_pubkey)
        .bind(&message.to_pubkey)
        .bind(&message.content)
        .bind(timestamp)
        .bind(&media_json)
        .bind(message.read as i32)
        .bind(message.delivered as i32)
        .bind(&message.reply_to)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(ReceivedDm::Stored)
    }

    /// The stranger limits of `receive_dm_message` for `message` from `peer_pubkey`.
    /// Returns false if the message is refused; evicts a stranger conversation if a new
    /// one would exceed `MAX_STRANGER_CONVERSATIONS`.
    async fn admit_stranger_message_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        peer_pubkey: &str,
        message: &StoredMessage,
    ) -> Result<bool, AppError> {
        let me = &message.to_pubkey;
        let conv_id = &message.conversation_id;
        let (exists, stranger): (bool, bool) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS (SELECT 1 FROM dm_conversations WHERE conversation_id = ?2),
                    NOT EXISTS (SELECT 1 FROM dm_conversations c
                        WHERE c.conversation_id = ?2 AND NOT ({STRANGER_CONVERSATION}))
                    AND NOT EXISTS (SELECT 1 FROM social_graph s
                        WHERE s.follower = ?1 AND s.followee = ?3
                          AND s.state = 'active' AND s.followed_at IS NOT NULL)"
        )))
        .bind(me)
        .bind(conv_id)
        .bind(peer_pubkey)
        .fetch_one(&mut **tx)
        .await?;
        if !stranger {
            return Ok(true);
        }
        if exists {
            let from_peer: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM dm_messages WHERE conversation_id = ?1 AND from_pubkey = ?2",
            )
            .bind(conv_id)
            .bind(peer_pubkey)
            .fetch_one(&mut **tx)
            .await?;
            return Ok(from_peer < MAX_STRANGER_MESSAGES);
        }
        let strangers: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM dm_conversations c WHERE {STRANGER_CONVERSATION}"
        )))
        .bind(me)
        .fetch_one(&mut **tx)
        .await?;
        if strangers < MAX_STRANGER_CONVERSATIONS {
            return Ok(true);
        }
        let evicted: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT c.conversation_id, c.peer_pubkey FROM dm_conversations c
             WHERE {STRANGER_CONVERSATION}
             ORDER BY c.last_message_at ASC, c.conversation_id ASC LIMIT ?2"
        )))
        .bind(me)
        .bind(strangers - MAX_STRANGER_CONVERSATIONS + 1)
        .fetch_all(&mut **tx)
        .await?;
        let mut peers = Vec::with_capacity(evicted.len());
        for (conversation, peer) in evicted {
            sqlx::query("DELETE FROM dm_messages WHERE conversation_id = ?1")
                .bind(&conversation)
                .execute(&mut **tx)
                .await?;
            sqlx::query("DELETE FROM dm_conversations WHERE conversation_id = ?1")
                .bind(&conversation)
                .execute(&mut **tx)
                .await?;
            peers.push(peer);
        }
        super::peer_identities::delete_unreferenced_identities_tx(tx, &peers).await?;
        Ok(true)
    }

    /// Ids of the newest `limit` messages by `(timestamp, id)`, newest first.
    pub async fn get_newest_dm_ids(&self, limit: usize) -> Result<Vec<String>, AppError> {
        Ok(sqlx::query_scalar(
            "SELECT id FROM dm_messages ORDER BY timestamp DESC, id DESC LIMIT ?1",
        )
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?)
    }

    /// The stored messages whose id is in `ids`, oldest first. Unknown ids are skipped.
    pub async fn get_dm_messages_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<StoredMessage>, AppError> {
        let rows = sqlx::query(
            "SELECT id, conversation_id, from_pubkey, to_pubkey, content, timestamp, media_json, read, delivered, reply_to
             FROM dm_messages WHERE id IN (SELECT value FROM json_each(?1))
             ORDER BY timestamp, id",
        )
        .bind(serde_json::to_string(ids)?)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Self::row_to_stored_message).collect()
    }
}
