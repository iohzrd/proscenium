use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::Storage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowRequestEntry {
    pub pubkey: String,
    pub timestamp: u64,
    pub status: String,
    pub created_at: u64,
    pub expires_at: u64,
}

impl Storage {
    pub fn insert_follow_request(
        &self,
        pubkey: &str,
        timestamp: u64,
        created_at: u64,
        expires_at: u64,
    ) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let existing: Option<String> = db
                .query_row(
                    "SELECT status FROM follow_requests WHERE pubkey=?1",
                    params![pubkey],
                    |row| row.get(0),
                )
                .ok();

            if existing.as_deref() == Some("approved") {
                return Ok(false);
            }

            db.execute(
                "INSERT INTO follow_requests (pubkey, timestamp, status, created_at, expires_at)
                 VALUES (?1, ?2, 'pending', ?3, ?4)
                 ON CONFLICT(pubkey) DO UPDATE SET timestamp=?2, status='pending', created_at=?3, expires_at=?4",
                params![pubkey, timestamp as i64, created_at as i64, expires_at as i64],
            )?;
            Ok(true)
        })
    }

    pub fn approve_follow_request(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let count = db.execute(
                "UPDATE follow_requests SET status='approved' WHERE pubkey=?1 AND status='pending'",
                params![pubkey],
            )?;
            Ok(count > 0)
        })
    }

    pub fn deny_follow_request(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let count = db.execute(
                "UPDATE follow_requests SET status='denied' WHERE pubkey=?1 AND status='pending'",
                params![pubkey],
            )?;
            Ok(count > 0)
        })
    }

    pub fn get_follow_requests(&self) -> anyhow::Result<Vec<FollowRequestEntry>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT pubkey, timestamp, status, created_at, expires_at
                 FROM follow_requests ORDER BY created_at DESC",
            )?;
            let mut rows = stmt.query([])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                entries.push(FollowRequestEntry {
                    pubkey: row.get(0)?,
                    timestamp: row.get::<_, i64>(1)? as u64,
                    status: row.get(2)?,
                    created_at: row.get::<_, i64>(3)? as u64,
                    expires_at: row.get::<_, i64>(4)? as u64,
                });
            }
            Ok(entries)
        })
    }

    pub fn get_pending_follow_request_count(&self) -> anyhow::Result<u64> {
        self.with_db(|db| {
            let count: i64 = db.query_row(
                "SELECT COUNT(*) FROM follow_requests WHERE status='pending'",
                [],
                |row| row.get(0),
            )?;
            Ok(count as u64)
        })
    }

    pub fn is_approved_follower(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let exists: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM follow_requests WHERE pubkey=?1 AND status='approved'",
                params![pubkey],
                |row| row.get(0),
            )?;
            Ok(exists)
        })
    }

    pub fn prune_expired_follow_requests(&self) -> anyhow::Result<u64> {
        self.with_db(|db| {
            let now = iroh_social_types::now_millis();
            let count = db.execute(
                "DELETE FROM follow_requests WHERE status='pending' AND expires_at < ?1",
                params![now as i64],
            )?;
            Ok(count as u64)
        })
    }
}
