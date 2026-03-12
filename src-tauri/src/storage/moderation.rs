use iroh_social_types::now_millis;
use sqlx::Row;

use super::Storage;

impl Storage {
    pub async fn toggle_bookmark(&self, post_id: &str) -> anyhow::Result<bool> {
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

    pub async fn is_bookmarked(&self, post_id: &str) -> anyhow::Result<bool> {
        let exists: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM bookmarks WHERE post_id=?1")
                .bind(post_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    pub async fn mute_user(&self, pubkey: &str) -> anyhow::Result<()> {
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO mutes (pubkey, created_at, state, last_changed_at) VALUES (?1, ?2, 'active', ?2)
             ON CONFLICT(pubkey) DO UPDATE SET state='active', last_changed_at=?2",
        )
        .bind(pubkey)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn unmute_user(&self, pubkey: &str) -> anyhow::Result<()> {
        let now = now_millis() as i64;
        sqlx::query("UPDATE mutes SET state='removed', last_changed_at=?2 WHERE pubkey=?1")
            .bind(pubkey)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn is_muted(&self, pubkey: &str) -> anyhow::Result<bool> {
        let exists: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM mutes WHERE pubkey=?1 AND state='active'")
                .bind(pubkey)
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    pub async fn get_muted_pubkeys(&self) -> anyhow::Result<Vec<String>> {
        let rows =
            sqlx::query("SELECT pubkey FROM mutes WHERE state='active' ORDER BY created_at DESC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn block_user(&self, pubkey: &str) -> anyhow::Result<()> {
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO blocks (pubkey, created_at, state, last_changed_at) VALUES (?1, ?2, 'active', ?2)
             ON CONFLICT(pubkey) DO UPDATE SET state='active', last_changed_at=?2",
        )
        .bind(pubkey)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn unblock_user(&self, pubkey: &str) -> anyhow::Result<()> {
        let now = now_millis() as i64;
        sqlx::query("UPDATE blocks SET state='removed', last_changed_at=?2 WHERE pubkey=?1")
            .bind(pubkey)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn is_blocked(&self, pubkey: &str) -> anyhow::Result<bool> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM blocks WHERE pubkey=?1 AND state='active'",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn get_blocked_pubkeys(&self) -> anyhow::Result<Vec<String>> {
        let rows =
            sqlx::query("SELECT pubkey FROM blocks WHERE state='active' ORDER BY created_at DESC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn is_hidden(&self, pubkey: &str) -> anyhow::Result<bool> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM mutes WHERE pubkey=?1 AND state='active')
                 OR EXISTS(SELECT 1 FROM blocks WHERE pubkey=?1 AND state='active')",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }
}
