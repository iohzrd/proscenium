use crate::constants::MAX_PENDING_FOLLOW_REQUESTS;
use crate::error::AppError;
use proscenium_types::FollowRequestEntry;
use sqlx::Row;

use super::Storage;

/// State of a requester's row after `record_follow_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowRequestOutcome {
    /// A pending row was inserted by this call.
    NewPending,
    /// A pending row already existed; it was left unchanged.
    StillPending,
    /// The requester is an approved follower.
    Approved,
    /// The local user denied this requester and the denial has not expired.
    Denied,
    /// No row was stored because `MAX_PENDING_FOLLOW_REQUESTS` pending rows exist.
    Full,
}

impl Storage {
    /// Record a verified follow request from `pubkey` and return the resulting row state.
    ///
    /// Runs in one write transaction. Pending and denied rows whose `expires_at` is not
    /// after `now` are deleted first. An approved or denied row is never changed, so a
    /// denied requester stays denied until the denial expires.
    pub async fn record_follow_request(
        &self,
        pubkey: &str,
        timestamp: u64,
        now: u64,
        expires_at: u64,
    ) -> Result<FollowRequestOutcome, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        sqlx::query("DELETE FROM follow_requests WHERE status != 'approved' AND expires_at <= ?1")
            .bind(now as i64)
            .execute(&mut *tx)
            .await?;

        let existing: Option<String> =
            sqlx::query_scalar("SELECT status FROM follow_requests WHERE pubkey=?1")
                .bind(pubkey)
                .fetch_optional(&mut *tx)
                .await?;
        let outcome = match existing.as_deref() {
            Some("approved") => FollowRequestOutcome::Approved,
            Some("denied") => FollowRequestOutcome::Denied,
            Some(_) => FollowRequestOutcome::StillPending,
            None => {
                let pending: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM follow_requests WHERE status='pending'",
                )
                .fetch_one(&mut *tx)
                .await?;
                if pending as u64 >= MAX_PENDING_FOLLOW_REQUESTS {
                    FollowRequestOutcome::Full
                } else {
                    sqlx::query(
                        "INSERT INTO follow_requests (pubkey, timestamp, status, created_at, expires_at)
                         VALUES (?1, ?2, 'pending', ?3, ?4)",
                    )
                    .bind(pubkey)
                    .bind(timestamp as i64)
                    .bind(now as i64)
                    .bind(expires_at as i64)
                    .execute(&mut *tx)
                    .await?;
                    FollowRequestOutcome::NewPending
                }
            }
        };

        tx.commit().await?;
        Ok(outcome)
    }

    /// Approve the pending, unexpired request of `pubkey` and record `pubkey` as a follower
    /// of `me`, in one transaction. Returns `false` if no such pending request exists.
    pub async fn approve_follow_request(
        &self,
        me: &str,
        pubkey: &str,
        now: u64,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let result = sqlx::query(
            "UPDATE follow_requests SET status='approved'
             WHERE pubkey=?1 AND status='pending' AND expires_at > ?2",
        )
        .bind(pubkey)
        .bind(now as i64)
        .execute(&mut *tx)
        .await?;
        let approved = result.rows_affected() > 0;
        if approved {
            Self::upsert_follower_tx(&mut tx, me, pubkey, now).await?;
        }
        tx.commit().await?;
        Ok(approved)
    }

    /// Store `pubkey` as an approved follower of `me` without a pending step, in one
    /// transaction. Used when a Private user receives a request from a user they follow.
    ///
    /// Returns `false` and writes nothing if an unexpired denied row exists for `pubkey`.
    pub async fn approve_follower_directly(
        &self,
        me: &str,
        pubkey: &str,
        timestamp: u64,
        now: u64,
        expires_at: u64,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let result = sqlx::query(
            "INSERT INTO follow_requests (pubkey, timestamp, status, created_at, expires_at)
             VALUES (?1, ?2, 'approved', ?3, ?4)
             ON CONFLICT(pubkey) DO UPDATE SET status='approved', timestamp=?2
             WHERE follow_requests.status != 'denied' OR follow_requests.expires_at <= ?3",
        )
        .bind(pubkey)
        .bind(timestamp as i64)
        .bind(now as i64)
        .bind(expires_at as i64)
        .execute(&mut *tx)
        .await?;
        let approved = result.rows_affected() > 0;
        if approved {
            Self::upsert_follower_tx(&mut tx, me, pubkey, now).await?;
        }
        tx.commit().await?;
        Ok(approved)
    }

    /// Deny the pending, unexpired request of `pubkey`.
    pub async fn deny_follow_request(&self, pubkey: &str, now: u64) -> Result<bool, AppError> {
        let result = sqlx::query(
            "UPDATE follow_requests SET status='denied'
             WHERE pubkey=?1 AND status='pending' AND expires_at > ?2",
        )
        .bind(pubkey)
        .bind(now as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// All approved requests plus the pending and denied requests that have not expired.
    pub async fn get_follow_requests(&self, now: u64) -> Result<Vec<FollowRequestEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT pubkey, timestamp, status, created_at, expires_at
             FROM follow_requests
             WHERE status='approved' OR expires_at > ?1
             ORDER BY created_at DESC, pubkey ASC",
        )
        .bind(now as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut entries = Vec::new();
        for row in &rows {
            entries.push(FollowRequestEntry {
                pubkey: row.get(0),
                timestamp: row.get::<i64, _>(1) as u64,
                status: row.get(2),
                created_at: row.get::<i64, _>(3) as u64,
                expires_at: row.get::<i64, _>(4) as u64,
            });
        }
        Ok(entries)
    }

    /// Number of pending requests that have not expired.
    pub async fn get_pending_follow_request_count(&self, now: u64) -> Result<u64, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM follow_requests WHERE status='pending' AND expires_at > ?1",
        )
        .bind(now as i64)
        .fetch_one(&self.pool)
        .await?;
        Ok(count as u64)
    }

    pub async fn is_approved_follower(&self, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM follow_requests WHERE pubkey=?1 AND status='approved'",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }
}
