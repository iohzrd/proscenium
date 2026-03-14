use crate::error::AppError;
use iroh_social_types::{FollowEntry, FollowerEntry, Visibility, now_millis};
use sqlx::Row;

use super::Storage;

impl Storage {
    pub async fn get_visibility(&self, pubkey: &str) -> Result<Visibility, AppError> {
        let result: Option<String> =
            sqlx::query_scalar("SELECT visibility FROM profiles WHERE pubkey=?1")
                .bind(pubkey)
                .fetch_optional(&self.pool)
                .await?;
        Ok(result
            .and_then(|s| s.parse().ok())
            .unwrap_or(Visibility::Public))
    }

    pub async fn is_follower(&self, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar("SELECT COUNT(*) > 0 FROM followers WHERE pubkey=?1")
            .bind(pubkey)
            .fetch_one(&self.pool)
            .await?;
        Ok(exists)
    }

    pub async fn follow(&self, entry: &FollowEntry) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO follows (pubkey, alias, followed_at, state, last_changed_at) VALUES (?1, ?2, ?3, 'active', ?4)
             ON CONFLICT(pubkey) DO UPDATE SET alias=?2, followed_at=?3, state='active', last_changed_at=?4",
        )
        .bind(&entry.pubkey)
        .bind(&entry.alias)
        .bind(entry.followed_at as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_follow_alias(
        &self,
        pubkey: &str,
        alias: Option<&str>,
    ) -> Result<(), AppError> {
        sqlx::query("UPDATE follows SET alias=?2 WHERE pubkey=?1")
            .bind(pubkey)
            .bind(alias)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn unfollow(&self, pubkey: &str) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query("UPDATE follows SET state='removed', last_changed_at=?2 WHERE pubkey=?1")
            .bind(pubkey)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_follows(&self) -> Result<Vec<FollowEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT pubkey, alias, followed_at FROM follows WHERE state='active' ORDER BY followed_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut follows = Vec::new();
        for row in &rows {
            follows.push(FollowEntry {
                pubkey: row.get(0),
                alias: row.get(1),
                followed_at: row.get::<i64, _>(2) as u64,
            });
        }
        Ok(follows)
    }

    pub async fn upsert_follower(&self, pubkey: &str, now: u64) -> Result<bool, AppError> {
        let existing: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM followers WHERE pubkey=?1")
                .bind(pubkey)
                .fetch_one(&self.pool)
                .await?;
        sqlx::query(
            "INSERT INTO followers (pubkey, first_seen, last_seen, is_online)
             VALUES (?1, ?2, ?2, 1)
             ON CONFLICT(pubkey) DO UPDATE SET last_seen=?2, is_online=1",
        )
        .bind(pubkey)
        .bind(now as i64)
        .execute(&self.pool)
        .await?;
        Ok(!existing)
    }

    pub async fn set_follower_offline(&self, pubkey: &str) -> Result<(), AppError> {
        sqlx::query("UPDATE followers SET is_online=0 WHERE pubkey=?1")
            .bind(pubkey)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_followers(&self) -> Result<Vec<FollowerEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT pubkey, first_seen, last_seen, is_online FROM followers ORDER BY last_seen DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut followers = Vec::new();
        for row in &rows {
            followers.push(FollowerEntry {
                pubkey: row.get(0),
                first_seen: row.get::<i64, _>(1) as u64,
                last_seen: row.get::<i64, _>(2) as u64,
                is_online: row.get::<i32, _>(3) != 0,
            });
        }
        Ok(followers)
    }

    pub async fn is_following(&self, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM follows WHERE pubkey=?1 AND state='active'",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn is_mutual(&self, pubkey: &str) -> Result<bool, AppError> {
        let is_follower: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM followers WHERE pubkey=?1")
                .bind(pubkey)
                .fetch_one(&self.pool)
                .await?;
        let is_following: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM follows WHERE pubkey=?1 AND state='active'",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(is_follower && is_following)
    }
}
