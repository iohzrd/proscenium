use crate::error::AppError;
use proscenium_types::now_millis;
use sqlx::Row;

use super::Storage;

impl Storage {
    pub async fn toggle_bookmark(&self, post_id: &str) -> Result<bool, AppError> {
        let exists: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM bookmarks WHERE post_id=?1")
                .bind(post_id)
                .fetch_one(&self.pool)
                .await?;
        if exists {
            sqlx::query("DELETE FROM bookmarks WHERE post_id=?1")
                .bind(post_id)
                .execute(&self.pool)
                .await?;
            Ok(false)
        } else {
            let now = now_millis() as i64;
            sqlx::query("INSERT INTO bookmarks (post_id, created_at) VALUES (?1, ?2)")
                .bind(post_id)
                .bind(now)
                .execute(&self.pool)
                .await?;
            Ok(true)
        }
    }

    pub async fn is_bookmarked(&self, post_id: &str) -> Result<bool, AppError> {
        let exists: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM bookmarks WHERE post_id=?1")
                .bind(post_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    pub async fn mute_user(&self, pubkey: &str) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO moderation (pubkey, kind, created_at, state, last_changed_at) VALUES (?1, 'mute', ?2, 'active', ?2)
             ON CONFLICT(pubkey, kind) DO UPDATE SET state='active', last_changed_at=?2",
        )
        .bind(pubkey)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn unmute_user(&self, pubkey: &str) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query(
            "UPDATE moderation SET state='removed', last_changed_at=?2 WHERE pubkey=?1 AND kind='mute'",
        )
        .bind(pubkey)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn is_muted(&self, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM moderation WHERE pubkey=?1 AND kind='mute' AND state='active'",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn get_muted_pubkeys(&self) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query(
            "SELECT pubkey FROM moderation WHERE kind='mute' AND state='active' ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn block_user(&self, pubkey: &str) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO moderation (pubkey, kind, created_at, state, last_changed_at) VALUES (?1, 'block', ?2, 'active', ?2)
             ON CONFLICT(pubkey, kind) DO UPDATE SET state='active', last_changed_at=?2",
        )
        .bind(pubkey)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn unblock_user(&self, pubkey: &str) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query(
            "UPDATE moderation SET state='removed', last_changed_at=?2 WHERE pubkey=?1 AND kind='block'",
        )
        .bind(pubkey)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn is_blocked(&self, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM moderation WHERE pubkey=?1 AND kind='block' AND state='active'",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn get_blocked_pubkeys(&self) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query(
            "SELECT pubkey FROM moderation WHERE kind='block' AND state='active' ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn is_hidden(&self, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM moderation WHERE pubkey=?1 AND state='active'",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }
}
