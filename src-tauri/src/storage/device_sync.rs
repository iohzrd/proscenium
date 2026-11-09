use crate::device_sync::LwwState;
use crate::error::AppError;
use proscenium_types::{
    BookmarkEntry, DeviceSyncVector, FollowEntry, MAX_DEVICE_SYNC_DM_IDS, MAX_SYNC_TOMBSTONES,
    ModerationEntry, is_valid_id, is_valid_pubkey, now_millis,
};

use super::Storage;

fn is_lww_state(state: &str) -> bool {
    matches!(state, "active" | "removed")
}

fn is_valid_follow_entry(me: &str, entry: &FollowEntry) -> bool {
    entry.pubkey != me && is_valid_pubkey(&entry.pubkey) && is_lww_state(&entry.state)
}

fn is_valid_moderation_entry(entry: &ModerationEntry) -> bool {
    is_valid_pubkey(&entry.pubkey)
        && matches!(entry.kind.as_str(), "mute" | "block")
        && is_lww_state(&entry.state)
}

fn is_valid_bookmark_entry(entry: &BookmarkEntry) -> bool {
    is_valid_id(&entry.post_id) && is_lww_state(&entry.state)
}

/// Convert a remote timestamp for storage. A value later than `now` is replaced by `now`,
/// so a device with a fast clock cannot store a row that outranks every later local change.
fn stored_timestamp(remote: u64, now: u64) -> i64 {
    i64::try_from(remote.min(now)).unwrap_or(i64::MAX)
}

impl Storage {
    /// Load the local user's follow, moderation and bookmark rows in key order.
    /// Rows that `merge_*_lww` on another device would skip are left out, so both
    /// devices compute `LwwState::digest` over the same set.
    pub async fn load_lww_state(&self, me: &str) -> Result<LwwState, AppError> {
        let follow_rows: Vec<(String, i64, String, i64)> = sqlx::query_as(
            "SELECT followee, followed_at, state, last_changed_at FROM social_graph
             WHERE follower = ?1 AND followed_at IS NOT NULL
             ORDER BY followee",
        )
        .bind(me)
        .fetch_all(&self.pool)
        .await?;
        let follows = follow_rows
            .into_iter()
            .map(
                |(pubkey, followed_at, state, last_changed_at)| FollowEntry {
                    pubkey,
                    followed_at: followed_at as u64,
                    state,
                    last_changed_at: last_changed_at as u64,
                },
            )
            .filter(|entry| is_valid_follow_entry(me, entry))
            .collect();

        let moderation_rows: Vec<(String, String, i64, String, i64)> = sqlx::query_as(
            "SELECT pubkey, kind, created_at, state, last_changed_at FROM moderation
             ORDER BY pubkey, kind",
        )
        .fetch_all(&self.pool)
        .await?;
        let moderation = moderation_rows
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
            .filter(is_valid_moderation_entry)
            .collect();

        let bookmark_rows: Vec<(String, i64, String, i64)> = sqlx::query_as(
            "SELECT post_id, created_at, state, last_changed_at FROM bookmarks ORDER BY post_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let bookmarks = bookmark_rows
            .into_iter()
            .map(
                |(post_id, created_at, state, last_changed_at)| BookmarkEntry {
                    post_id,
                    created_at: created_at as u64,
                    state,
                    last_changed_at: last_changed_at as u64,
                },
            )
            .filter(is_valid_bookmark_entry)
            .collect();

        Ok(LwwState {
            follows,
            moderation,
            bookmarks,
        })
    }

    /// Build the summary a device sends at the start of a device sync.
    pub async fn build_device_sync_vector(
        &self,
        master_pubkey: &str,
        lww: &LwwState,
    ) -> Result<DeviceSyncVector, AppError> {
        let (post_count, newest_post_ts): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(MAX(timestamp), 0) FROM posts WHERE author = ?1",
        )
        .bind(master_pubkey)
        .fetch_one(&self.pool)
        .await?;
        let (interaction_count, newest_interaction_ts): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(MAX(timestamp), 0) FROM interactions WHERE author = ?1",
        )
        .bind(master_pubkey)
        .fetch_one(&self.pool)
        .await?;

        Ok(DeviceSyncVector {
            post_count: post_count as u64,
            newest_post_ts: newest_post_ts as u64,
            interaction_count: interaction_count as u64,
            newest_interaction_ts: newest_interaction_ts as u64,
            lww_digest: lww.digest(),
            devices: self.get_linked_devices().await?,
            dm_ids: self.get_newest_dm_ids(MAX_DEVICE_SYNC_DM_IDS).await?,
            post_tombstone_ids: self
                .get_post_tombstones(master_pubkey, MAX_SYNC_TOMBSTONES)
                .await?
                .into_iter()
                .map(|tombstone| tombstone.id)
                .collect(),
            interaction_tombstone_ids: self
                .get_interaction_tombstone_ids(master_pubkey, MAX_SYNC_TOMBSTONES)
                .await?,
        })
    }

    /// Merge follow rows received from another device of the local user `me`.
    ///
    /// A received row replaces the local row only if its `last_changed_at` is greater.
    /// Invalid entries are skipped. Returns the number of rows written.
    pub async fn merge_follows_lww(
        &self,
        me: &str,
        entries: &[FollowEntry],
    ) -> Result<u32, AppError> {
        let now = now_millis();
        let mut merged = 0u32;
        for entry in entries {
            if !is_valid_follow_entry(me, entry) {
                continue;
            }
            let result = sqlx::query(
                "INSERT INTO social_graph (follower, followee, followed_at, state, last_changed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(follower, followee) DO UPDATE SET
                    followed_at = excluded.followed_at,
                    state = excluded.state,
                    last_changed_at = excluded.last_changed_at
                 WHERE excluded.last_changed_at > social_graph.last_changed_at",
            )
            .bind(me)
            .bind(&entry.pubkey)
            .bind(stored_timestamp(entry.followed_at, now))
            .bind(&entry.state)
            .bind(stored_timestamp(entry.last_changed_at, now))
            .execute(&self.pool)
            .await?;
            if result.rows_affected() > 0 {
                merged += 1;
            }
        }
        Ok(merged)
    }

    /// Merge mute and block rows received from another device of the local user.
    /// Same replacement rule as `merge_follows_lww`.
    pub async fn merge_moderation_lww(&self, entries: &[ModerationEntry]) -> Result<u32, AppError> {
        let now = now_millis();
        let mut merged = 0u32;
        for entry in entries {
            if !is_valid_moderation_entry(entry) {
                continue;
            }
            let result = sqlx::query(
                "INSERT INTO moderation (pubkey, kind, created_at, state, last_changed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(pubkey, kind) DO UPDATE SET
                    created_at = excluded.created_at,
                    state = excluded.state,
                    last_changed_at = excluded.last_changed_at
                 WHERE excluded.last_changed_at > moderation.last_changed_at",
            )
            .bind(&entry.pubkey)
            .bind(&entry.kind)
            .bind(stored_timestamp(entry.created_at, now))
            .bind(&entry.state)
            .bind(stored_timestamp(entry.last_changed_at, now))
            .execute(&self.pool)
            .await?;
            if result.rows_affected() > 0 {
                merged += 1;
            }
        }
        Ok(merged)
    }

    /// Merge bookmark rows received from another device of the local user.
    /// Same replacement rule as `merge_follows_lww`.
    pub async fn merge_bookmarks_lww(&self, entries: &[BookmarkEntry]) -> Result<u32, AppError> {
        let now = now_millis();
        let mut merged = 0u32;
        for entry in entries {
            if !is_valid_bookmark_entry(entry) {
                continue;
            }
            let result = sqlx::query(
                "INSERT INTO bookmarks (post_id, created_at, state, last_changed_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(post_id) DO UPDATE SET
                    created_at = excluded.created_at,
                    state = excluded.state,
                    last_changed_at = excluded.last_changed_at
                 WHERE excluded.last_changed_at > bookmarks.last_changed_at",
            )
            .bind(&entry.post_id)
            .bind(stored_timestamp(entry.created_at, now))
            .bind(&entry.state)
            .bind(stored_timestamp(entry.last_changed_at, now))
            .execute(&self.pool)
            .await?;
            if result.rows_affected() > 0 {
                merged += 1;
            }
        }
        Ok(merged)
    }
}
