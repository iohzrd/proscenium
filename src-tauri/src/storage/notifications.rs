use crate::error::AppError;
use sha2::{Digest, Sha256};
use sqlx::Row;

use proscenium_types::Notification;

use super::{Cursor, Storage, page_limit, sql_timestamp};

/// Largest number of rows kept in `notifications`. Rows are created from remote input,
/// so the table is cut back to this size on every insert.
pub(super) const MAX_NOTIFICATIONS: i64 = 5_000;

impl Storage {
    /// Insert a notification unless one with the same kind, actor and post ids exists,
    /// then delete the oldest rows beyond `MAX_NOTIFICATIONS`.
    pub async fn insert_notification(
        &self,
        kind: &str,
        actor: &str,
        target_post_id: Option<&str>,
        post_id: Option<&str>,
        timestamp: u64,
    ) -> Result<(), AppError> {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update(actor.as_bytes());
        hasher.update(target_post_id.unwrap_or("").as_bytes());
        hasher.update(post_id.unwrap_or("").as_bytes());
        let id = hex::encode(hasher.finalize());

        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "INSERT OR IGNORE INTO notifications (id, kind, actor, target_post_id, post_id, timestamp, read)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        )
        .bind(&id)
        .bind(kind)
        .bind(actor)
        .bind(target_post_id)
        .bind(post_id)
        .bind(sql_timestamp(timestamp))
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() > 0 {
            sqlx::query(
                "DELETE FROM notifications WHERE id IN (
                     SELECT id FROM notifications
                     ORDER BY timestamp DESC, id DESC LIMIT -1 OFFSET ?1)",
            )
            .bind(MAX_NOTIFICATIONS)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Notifications without muted and blocked actors, ordered by `(timestamp, id)`
    /// descending. `before` selects the rows after that position in this order.
    pub async fn get_notifications(
        &self,
        limit: usize,
        before: Option<Cursor<'_>>,
    ) -> Result<Vec<Notification>, AppError> {
        let mut sql = String::from(
            "SELECT id, kind, actor, target_post_id, post_id, timestamp, read
             FROM notifications
             WHERE actor NOT IN (SELECT pubkey FROM moderation WHERE state='active')",
        );
        if before.is_some() {
            sql.push_str(" AND (timestamp, id) < (?2, ?3)");
        }
        sql.push_str(" ORDER BY timestamp DESC, id DESC LIMIT ?1");

        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(page_limit(limit));
        if let Some((timestamp, id)) = before {
            query = query.bind(sql_timestamp(timestamp)).bind(id);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.iter().map(Self::row_to_notification).collect())
    }

    fn row_to_notification(row: &sqlx::sqlite::SqliteRow) -> Notification {
        let read_int: i32 = row.get(6);
        Notification {
            id: row.get(0),
            kind: row.get(1),
            actor: row.get(2),
            target_post_id: row.get(3),
            post_id: row.get(4),
            timestamp: row.get::<i64, _>(5) as u64,
            read: read_int != 0,
        }
    }

    /// Unread notifications, without those of muted or blocked actors, which
    /// `get_notifications` does not return either.
    pub async fn get_unread_notification_count(&self) -> Result<u32, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notifications
             WHERE read=0 AND actor NOT IN (SELECT pubkey FROM moderation WHERE state='active')",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(count as u32)
    }

    pub async fn mark_notifications_read(&self) -> Result<(), AppError> {
        sqlx::query("UPDATE notifications SET read=1 WHERE read=0")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
