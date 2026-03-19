use crate::error::AppError;
use proscenium_types::{
    DeviceSyncVector, FollowEntry, ModerationEntry, RatchetSessionExport, RatchetSyncEntry,
};
use sqlx::Row;

use super::Storage;

impl Storage {
    pub async fn build_device_sync_vector(
        &self,
        master_pubkey: &str,
    ) -> Result<DeviceSyncVector, AppError> {
        let post_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE author = ?1")
            .bind(master_pubkey)
            .fetch_one(&self.pool)
            .await?;
        let newest_post_ts: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(timestamp), 0) FROM posts WHERE author = ?1")
                .bind(master_pubkey)
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let interaction_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM interactions WHERE author = ?1")
                .bind(master_pubkey)
                .fetch_one(&self.pool)
                .await?;
        let newest_interaction_ts: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(timestamp), 0) FROM interactions WHERE author = ?1",
        )
        .bind(master_pubkey)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        // Full follow list with LWW timestamps
        let follow_rows: Vec<(String, i64, String, i64)> = sqlx::query_as(
            "SELECT followee, followed_at, state, last_changed_at FROM social_graph
             WHERE follower = ?1 AND followed_at IS NOT NULL",
        )
        .bind(master_pubkey)
        .fetch_all(&self.pool)
        .await?;
        let follows: Vec<FollowEntry> = follow_rows
            .into_iter()
            .map(
                |(pubkey, followed_at, state, last_changed_at)| FollowEntry {
                    pubkey,
                    followed_at: followed_at as u64,
                    state,
                    last_changed_at: last_changed_at as u64,
                },
            )
            .collect();

        // Full moderation list (mutes + blocks) with LWW timestamps
        let mod_rows: Vec<(String, String, i64, String, i64)> = sqlx::query_as(
            "SELECT pubkey, kind, created_at, state, last_changed_at FROM moderation",
        )
        .fetch_all(&self.pool)
        .await?;
        let moderation: Vec<ModerationEntry> = mod_rows
            .into_iter()
            .map(
                |(pubkey, kind, created_at, state, last_changed_at)| ModerationEntry {
                    pubkey,
                    kind,
                    created_at: created_at as u64,
                    state,
                    last_changed_at: last_changed_at as u64,
                },
            )
            .collect();

        // All bookmark IDs
        let bookmark_rows = sqlx::query("SELECT post_id FROM bookmarks ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        let bookmarks: Vec<String> = bookmark_rows.iter().map(|r| r.get(0)).collect();

        // Ratchet session summaries
        let ratchet_rows = sqlx::query("SELECT peer_pubkey, updated_at FROM dm_ratchet_sessions")
            .fetch_all(&self.pool)
            .await?;
        let ratchet_summaries: Vec<RatchetSyncEntry> = ratchet_rows
            .iter()
            .map(|row| RatchetSyncEntry {
                peer_pubkey: row.get(0),
                updated_at: row.get::<i64, _>(1) as u64,
            })
            .collect();

        // Newest DM message timestamp
        let dm_newest_ts: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(timestamp), 0) FROM dm_messages")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        Ok(DeviceSyncVector {
            post_count: post_count as u64,
            newest_post_ts: newest_post_ts as u64,
            interaction_count: interaction_count as u64,
            newest_interaction_ts: newest_interaction_ts as u64,
            follows,
            moderation,
            bookmarks,
            ratchet_summaries,
            dm_newest_ts: dm_newest_ts as u64,
        })
    }

    pub async fn merge_follows_lww(
        &self,
        me: &str,
        entries: &[FollowEntry],
    ) -> Result<u32, AppError> {
        let mut merged = 0u32;
        for entry in entries {
            let local_changed: Option<i64> = sqlx::query_scalar(
                "SELECT last_changed_at FROM social_graph WHERE follower = ?1 AND followee = ?2",
            )
            .bind(me)
            .bind(&entry.pubkey)
            .fetch_optional(&self.pool)
            .await?;

            let should_update = match local_changed {
                Some(local_ts) => entry.last_changed_at > local_ts as u64,
                None => true,
            };

            if should_update {
                sqlx::query(
                    "INSERT INTO social_graph (follower, followee, followed_at, state, last_changed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(follower, followee) DO UPDATE SET
                        followed_at = ?3, state = ?4, last_changed_at = ?5",
                )
                .bind(me)
                .bind(&entry.pubkey)
                .bind(entry.followed_at as i64)
                .bind(&entry.state)
                .bind(entry.last_changed_at as i64)
                .execute(&self.pool)
                .await?;
                merged += 1;
            }
        }
        Ok(merged)
    }

    pub async fn merge_moderation_lww(&self, entries: &[ModerationEntry]) -> Result<u32, AppError> {
        let mut merged = 0u32;
        for entry in entries {
            let local_changed: Option<i64> = sqlx::query_scalar(
                "SELECT last_changed_at FROM moderation WHERE pubkey = ?1 AND kind = ?2",
            )
            .bind(&entry.pubkey)
            .bind(&entry.kind)
            .fetch_optional(&self.pool)
            .await?;

            let should_update = match local_changed {
                Some(local_ts) => entry.last_changed_at > local_ts as u64,
                None => true,
            };

            if should_update {
                sqlx::query(
                    "INSERT INTO moderation (pubkey, kind, created_at, state, last_changed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(pubkey, kind) DO UPDATE SET
                        created_at = ?3, state = ?4, last_changed_at = ?5",
                )
                .bind(&entry.pubkey)
                .bind(&entry.kind)
                .bind(entry.created_at as i64)
                .bind(&entry.state)
                .bind(entry.last_changed_at as i64)
                .execute(&self.pool)
                .await?;
                merged += 1;
            }
        }
        Ok(merged)
    }

    pub async fn merge_bookmarks(&self, post_ids: &[String]) -> Result<u32, AppError> {
        let mut added = 0u32;
        let now = proscenium_types::now_millis() as i64;
        for post_id in post_ids {
            let exists: bool =
                sqlx::query_scalar("SELECT COUNT(*) > 0 FROM bookmarks WHERE post_id = ?1")
                    .bind(post_id)
                    .fetch_one(&self.pool)
                    .await?;
            if !exists {
                sqlx::query("INSERT INTO bookmarks (post_id, created_at) VALUES (?1, ?2)")
                    .bind(post_id)
                    .bind(now)
                    .execute(&self.pool)
                    .await?;
                added += 1;
            }
        }
        Ok(added)
    }

    pub async fn merge_ratchet_sessions_lww(
        &self,
        sessions: &[RatchetSessionExport],
    ) -> Result<u32, AppError> {
        let mut merged = 0u32;
        for session in sessions {
            let local_updated: Option<i64> = sqlx::query_scalar(
                "SELECT updated_at FROM dm_ratchet_sessions WHERE peer_pubkey = ?1",
            )
            .bind(&session.peer_pubkey)
            .fetch_optional(&self.pool)
            .await?;

            let remote_ts = session.updated_at as i64;
            let should_update = match local_updated {
                Some(local_ts) => remote_ts > local_ts,
                None => true,
            };

            if should_update {
                sqlx::query(
                    "INSERT INTO dm_ratchet_sessions (peer_pubkey, state_json, updated_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(peer_pubkey) DO UPDATE SET state_json = ?2, updated_at = ?3",
                )
                .bind(&session.peer_pubkey)
                .bind(&session.state_json)
                .bind(remote_ts)
                .execute(&self.pool)
                .await?;
                merged += 1;
            }
        }
        Ok(merged)
    }
}
