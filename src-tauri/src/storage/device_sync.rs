use iroh_social_types::{
    DeviceSyncVector, FollowSyncEntry, ModerationSyncEntry, RatchetSessionExport, RatchetSyncEntry,
};
use sqlx::Row;

use super::Storage;

impl Storage {
    pub async fn build_device_sync_vector(
        &self,
        master_pubkey: &str,
    ) -> anyhow::Result<DeviceSyncVector> {
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
        let follow_rows =
            sqlx::query("SELECT pubkey, alias, followed_at, state, last_changed_at FROM follows")
                .fetch_all(&self.pool)
                .await?;
        let follows: Vec<FollowSyncEntry> = follow_rows
            .iter()
            .map(|row| FollowSyncEntry {
                pubkey: row.get(0),
                alias: row.get(1),
                followed_at: row.get::<i64, _>(2) as u64,
                state: row.get(3),
                last_changed_at: row.get::<i64, _>(4) as u64,
            })
            .collect();

        // Full mute list with LWW timestamps
        let mute_rows = sqlx::query("SELECT pubkey, created_at, state, last_changed_at FROM mutes")
            .fetch_all(&self.pool)
            .await?;
        let mutes: Vec<ModerationSyncEntry> = mute_rows
            .iter()
            .map(|row| ModerationSyncEntry {
                pubkey: row.get(0),
                created_at: row.get::<i64, _>(1) as u64,
                state: row.get(2),
                last_changed_at: row.get::<i64, _>(3) as u64,
            })
            .collect();

        // Full block list with LWW timestamps
        let block_rows =
            sqlx::query("SELECT pubkey, created_at, state, last_changed_at FROM blocks")
                .fetch_all(&self.pool)
                .await?;
        let blocks: Vec<ModerationSyncEntry> = block_rows
            .iter()
            .map(|row| ModerationSyncEntry {
                pubkey: row.get(0),
                created_at: row.get::<i64, _>(1) as u64,
                state: row.get(2),
                last_changed_at: row.get::<i64, _>(3) as u64,
            })
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
            mutes,
            blocks,
            bookmarks,
            ratchet_summaries,
            dm_newest_ts: dm_newest_ts as u64,
        })
    }

    pub async fn merge_follows_lww(&self, entries: &[FollowSyncEntry]) -> anyhow::Result<u32> {
        let mut merged = 0u32;
        for entry in entries {
            let local_changed: Option<i64> =
                sqlx::query_scalar("SELECT last_changed_at FROM follows WHERE pubkey = ?1")
                    .bind(&entry.pubkey)
                    .fetch_optional(&self.pool)
                    .await?;

            let should_update = match local_changed {
                Some(local_ts) => entry.last_changed_at > local_ts as u64,
                None => true,
            };

            if should_update {
                sqlx::query(
                    "INSERT INTO follows (pubkey, alias, followed_at, state, last_changed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(pubkey) DO UPDATE SET
                        alias = ?2, followed_at = ?3, state = ?4, last_changed_at = ?5",
                )
                .bind(&entry.pubkey)
                .bind(&entry.alias)
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

    pub async fn merge_mutes_lww(&self, entries: &[ModerationSyncEntry]) -> anyhow::Result<u32> {
        let mut merged = 0u32;
        for entry in entries {
            let local_changed: Option<i64> =
                sqlx::query_scalar("SELECT last_changed_at FROM mutes WHERE pubkey = ?1")
                    .bind(&entry.pubkey)
                    .fetch_optional(&self.pool)
                    .await?;

            let should_update = match local_changed {
                Some(local_ts) => entry.last_changed_at > local_ts as u64,
                None => true,
            };

            if should_update {
                sqlx::query(
                    "INSERT INTO mutes (pubkey, created_at, state, last_changed_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(pubkey) DO UPDATE SET
                        created_at = ?2, state = ?3, last_changed_at = ?4",
                )
                .bind(&entry.pubkey)
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

    pub async fn merge_blocks_lww(&self, entries: &[ModerationSyncEntry]) -> anyhow::Result<u32> {
        let mut merged = 0u32;
        for entry in entries {
            let local_changed: Option<i64> =
                sqlx::query_scalar("SELECT last_changed_at FROM blocks WHERE pubkey = ?1")
                    .bind(&entry.pubkey)
                    .fetch_optional(&self.pool)
                    .await?;

            let should_update = match local_changed {
                Some(local_ts) => entry.last_changed_at > local_ts as u64,
                None => true,
            };

            if should_update {
                sqlx::query(
                    "INSERT INTO blocks (pubkey, created_at, state, last_changed_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(pubkey) DO UPDATE SET
                        created_at = ?2, state = ?3, last_changed_at = ?4",
                )
                .bind(&entry.pubkey)
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

    pub async fn merge_bookmarks(&self, post_ids: &[String]) -> anyhow::Result<u32> {
        let mut added = 0u32;
        let now = iroh_social_types::now_millis() as i64;
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
    ) -> anyhow::Result<u32> {
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
