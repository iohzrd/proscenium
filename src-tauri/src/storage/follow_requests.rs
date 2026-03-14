use crate::error::AppError;
use iroh_social_types::FollowRequestEntry;
use sqlx::Row;

use super::Storage;

impl Storage {
    pub async fn insert_follow_request(
        &self,
        pubkey: &str,
        timestamp: u64,
        created_at: u64,
        expires_at: u64,
    ) -> Result<bool, AppError> {
        let existing: Option<String> =
            sqlx::query_scalar("SELECT status FROM follow_requests WHERE pubkey=?1")
                .bind(pubkey)
                .fetch_optional(&self.pool)
                .await?;

        if existing.as_deref() == Some("approved") {
            return Ok(false);
        }

        sqlx::query(
            "INSERT INTO follow_requests (pubkey, timestamp, status, created_at, expires_at)
             VALUES (?1, ?2, 'pending', ?3, ?4)
             ON CONFLICT(pubkey) DO UPDATE SET timestamp=?2, status='pending', created_at=?3, expires_at=?4",
        )
        .bind(pubkey)
        .bind(timestamp as i64)
        .bind(created_at as i64)
        .bind(expires_at as i64)
        .execute(&self.pool)
        .await?;
        Ok(true)
    }

    pub async fn approve_follow_request(&self, pubkey: &str) -> Result<bool, AppError> {
        let result = sqlx::query(
            "UPDATE follow_requests SET status='approved' WHERE pubkey=?1 AND status='pending'",
        )
        .bind(pubkey)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn deny_follow_request(&self, pubkey: &str) -> Result<bool, AppError> {
        let result = sqlx::query(
            "UPDATE follow_requests SET status='denied' WHERE pubkey=?1 AND status='pending'",
        )
        .bind(pubkey)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_follow_requests(&self) -> Result<Vec<FollowRequestEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT pubkey, timestamp, status, created_at, expires_at
             FROM follow_requests ORDER BY created_at DESC",
        )
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

    pub async fn get_pending_follow_request_count(&self) -> Result<u64, AppError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM follow_requests WHERE status='pending'")
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
