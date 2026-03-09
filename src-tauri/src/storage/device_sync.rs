use iroh_social_types::{
    DeviceSyncVector, FollowSyncEntry, ModerationSyncEntry, RatchetSessionExport, RatchetSyncEntry,
};
use rusqlite::params;

use super::Storage;

impl Storage {
    /// Build a sync vector summarizing local state for device sync.
    pub fn build_device_sync_vector(
        &self,
        master_pubkey: &str,
    ) -> anyhow::Result<DeviceSyncVector> {
        self.with_db(|db| {
            let post_count: i64 = db.query_row(
                "SELECT COUNT(*) FROM posts WHERE author = ?1",
                params![master_pubkey],
                |row| row.get(0),
            )?;
            let newest_post_ts: i64 = db
                .query_row(
                    "SELECT COALESCE(MAX(timestamp), 0) FROM posts WHERE author = ?1",
                    params![master_pubkey],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let interaction_count: i64 = db.query_row(
                "SELECT COUNT(*) FROM interactions WHERE author = ?1",
                params![master_pubkey],
                |row| row.get(0),
            )?;
            let newest_interaction_ts: i64 = db
                .query_row(
                    "SELECT COALESCE(MAX(timestamp), 0) FROM interactions WHERE author = ?1",
                    params![master_pubkey],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            // Full follow list with LWW timestamps
            let follows = {
                let mut stmt = db.prepare(
                    "SELECT pubkey, alias, followed_at, state, last_changed_at FROM follows",
                )?;
                let mut rows = stmt.query([])?;
                let mut entries = Vec::new();
                while let Some(row) = rows.next()? {
                    entries.push(FollowSyncEntry {
                        pubkey: row.get(0)?,
                        alias: row.get(1)?,
                        followed_at: row.get::<_, i64>(2)? as u64,
                        state: row.get(3)?,
                        last_changed_at: row.get::<_, i64>(4)? as u64,
                    });
                }
                entries
            };

            // Full mute list with LWW timestamps
            let mutes = {
                let mut stmt =
                    db.prepare("SELECT pubkey, created_at, state, last_changed_at FROM mutes")?;
                let mut rows = stmt.query([])?;
                let mut entries = Vec::new();
                while let Some(row) = rows.next()? {
                    entries.push(ModerationSyncEntry {
                        pubkey: row.get(0)?,
                        created_at: row.get::<_, i64>(1)? as u64,
                        state: row.get(2)?,
                        last_changed_at: row.get::<_, i64>(3)? as u64,
                    });
                }
                entries
            };

            // Full block list with LWW timestamps
            let blocks = {
                let mut stmt =
                    db.prepare("SELECT pubkey, created_at, state, last_changed_at FROM blocks")?;
                let mut rows = stmt.query([])?;
                let mut entries = Vec::new();
                while let Some(row) = rows.next()? {
                    entries.push(ModerationSyncEntry {
                        pubkey: row.get(0)?,
                        created_at: row.get::<_, i64>(1)? as u64,
                        state: row.get(2)?,
                        last_changed_at: row.get::<_, i64>(3)? as u64,
                    });
                }
                entries
            };

            // All bookmark IDs
            let bookmarks = {
                let mut stmt =
                    db.prepare("SELECT post_id FROM bookmarks ORDER BY created_at DESC")?;
                let mut rows = stmt.query([])?;
                let mut ids = Vec::new();
                while let Some(row) = rows.next()? {
                    ids.push(row.get(0)?);
                }
                ids
            };

            // Ratchet session summaries
            let ratchet_summaries = {
                let mut stmt =
                    db.prepare("SELECT peer_pubkey, updated_at FROM dm_ratchet_sessions")?;
                let mut rows = stmt.query([])?;
                let mut entries = Vec::new();
                while let Some(row) = rows.next()? {
                    entries.push(RatchetSyncEntry {
                        peer_pubkey: row.get(0)?,
                        updated_at: row.get::<_, i64>(1)? as u64,
                    });
                }
                entries
            };

            // Newest DM message timestamp
            let dm_newest_ts: i64 = db
                .query_row(
                    "SELECT COALESCE(MAX(timestamp), 0) FROM dm_messages",
                    [],
                    |row| row.get(0),
                )
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
        })
    }

    /// Merge incoming follow entries using LWW semantics.
    pub fn merge_follows_lww(&self, entries: &[FollowSyncEntry]) -> anyhow::Result<u32> {
        let mut merged = 0u32;
        self.with_db(|db| {
            for entry in entries {
                let local_changed: Option<i64> = db
                    .query_row(
                        "SELECT last_changed_at FROM follows WHERE pubkey = ?1",
                        params![entry.pubkey],
                        |row| row.get(0),
                    )
                    .ok();

                let should_update = match local_changed {
                    Some(local_ts) => entry.last_changed_at > local_ts as u64,
                    None => true,
                };

                if should_update {
                    db.execute(
                        "INSERT INTO follows (pubkey, alias, followed_at, state, last_changed_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(pubkey) DO UPDATE SET
                            alias = ?2, followed_at = ?3, state = ?4, last_changed_at = ?5",
                        params![
                            entry.pubkey,
                            entry.alias,
                            entry.followed_at as i64,
                            entry.state,
                            entry.last_changed_at as i64,
                        ],
                    )?;
                    merged += 1;
                }
            }
            Ok(merged)
        })
    }

    /// Merge incoming mute entries using LWW semantics.
    pub fn merge_mutes_lww(&self, entries: &[ModerationSyncEntry]) -> anyhow::Result<u32> {
        let mut merged = 0u32;
        self.with_db(|db| {
            for entry in entries {
                let local_changed: Option<i64> = db
                    .query_row(
                        "SELECT last_changed_at FROM mutes WHERE pubkey = ?1",
                        params![entry.pubkey],
                        |row| row.get(0),
                    )
                    .ok();

                let should_update = match local_changed {
                    Some(local_ts) => entry.last_changed_at > local_ts as u64,
                    None => true,
                };

                if should_update {
                    db.execute(
                        "INSERT INTO mutes (pubkey, created_at, state, last_changed_at)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(pubkey) DO UPDATE SET
                            created_at = ?2, state = ?3, last_changed_at = ?4",
                        params![
                            entry.pubkey,
                            entry.created_at as i64,
                            entry.state,
                            entry.last_changed_at as i64,
                        ],
                    )?;
                    merged += 1;
                }
            }
            Ok(merged)
        })
    }

    /// Merge incoming block entries using LWW semantics.
    pub fn merge_blocks_lww(&self, entries: &[ModerationSyncEntry]) -> anyhow::Result<u32> {
        let mut merged = 0u32;
        self.with_db(|db| {
            for entry in entries {
                let local_changed: Option<i64> = db
                    .query_row(
                        "SELECT last_changed_at FROM blocks WHERE pubkey = ?1",
                        params![entry.pubkey],
                        |row| row.get(0),
                    )
                    .ok();

                let should_update = match local_changed {
                    Some(local_ts) => entry.last_changed_at > local_ts as u64,
                    None => true,
                };

                if should_update {
                    db.execute(
                        "INSERT INTO blocks (pubkey, created_at, state, last_changed_at)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(pubkey) DO UPDATE SET
                            created_at = ?2, state = ?3, last_changed_at = ?4",
                        params![
                            entry.pubkey,
                            entry.created_at as i64,
                            entry.state,
                            entry.last_changed_at as i64,
                        ],
                    )?;
                    merged += 1;
                }
            }
            Ok(merged)
        })
    }

    /// Merge bookmarks from another device (set union).
    pub fn merge_bookmarks(&self, post_ids: &[String]) -> anyhow::Result<u32> {
        let mut added = 0u32;
        self.with_db(|db| {
            let now = iroh_social_types::now_millis() as i64;
            for post_id in post_ids {
                let exists: bool = db.query_row(
                    "SELECT COUNT(*) > 0 FROM bookmarks WHERE post_id = ?1",
                    params![post_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    db.execute(
                        "INSERT INTO bookmarks (post_id, created_at) VALUES (?1, ?2)",
                        params![post_id, now],
                    )?;
                    added += 1;
                }
            }
            Ok(added)
        })
    }

    /// Merge ratchet sessions using LWW by updated_at.
    pub fn merge_ratchet_sessions_lww(
        &self,
        sessions: &[RatchetSessionExport],
    ) -> anyhow::Result<u32> {
        let mut merged = 0u32;
        self.with_db(|db| {
            for session in sessions {
                let local_updated: Option<i64> = db
                    .query_row(
                        "SELECT updated_at FROM dm_ratchet_sessions WHERE peer_pubkey = ?1",
                        params![session.peer_pubkey],
                        |row| row.get(0),
                    )
                    .ok();

                // For ratchet sessions, we only accept state with a newer updated_at
                let remote_ts = session.updated_at as i64;
                let should_update = match local_updated {
                    Some(local_ts) => remote_ts > local_ts,
                    None => true,
                };

                if should_update {
                    db.execute(
                        "INSERT INTO dm_ratchet_sessions (peer_pubkey, state_json, updated_at)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(peer_pubkey) DO UPDATE SET state_json = ?2, updated_at = ?3",
                        params![session.peer_pubkey, session.state_json, remote_ts],
                    )?;
                    merged += 1;
                }
            }
            Ok(merged)
        })
    }

    /// Get transport NodeIds for other linked devices (not self).
    pub fn get_other_device_node_ids(&self) -> anyhow::Result<Vec<String>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT node_id FROM linked_devices WHERE is_self = 0 ORDER BY added_at ASC",
            )?;
            let mut rows = stmt.query([])?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next()? {
                ids.push(row.get(0)?);
            }
            Ok(ids)
        })
    }
}
