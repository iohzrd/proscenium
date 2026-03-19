use crate::error::AppError;
use proscenium_types::{SocialGraphEntry, Visibility, now_millis};

use super::Storage;

/// Cached remote social graph result.
pub struct CachedRemoteSocial {
    pub entries: Vec<SocialGraphEntry>,
    pub hidden: bool,
    pub fetched_at: Option<u64>,
}

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

    // ---- Follows ----

    pub async fn follow(&self, me: &str, entry: &SocialGraphEntry) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO social_graph (follower, followee, followed_at, state, last_changed_at)
             VALUES (?1, ?2, ?3, 'active', ?4)
             ON CONFLICT(follower, followee) DO UPDATE SET followed_at=?3, state='active', last_changed_at=?4",
        )
        .bind(me)
        .bind(&entry.pubkey)
        .bind(entry.followed_at as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn unfollow(&self, me: &str, pubkey: &str) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query(
            "UPDATE social_graph SET state='removed', last_changed_at=?3
             WHERE follower=?1 AND followee=?2",
        )
        .bind(me)
        .bind(pubkey)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_follows(&self, me: &str) -> Result<Vec<SocialGraphEntry>, AppError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT followee, followed_at FROM social_graph
             WHERE follower=?1 AND state='active' ORDER BY followed_at DESC",
        )
        .bind(me)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(pubkey, followed_at)| SocialGraphEntry {
                pubkey,
                followed_at: followed_at as u64,
                first_seen: 0,
                last_seen: 0,
                is_online: false,
            })
            .collect())
    }

    pub async fn is_following(&self, me: &str, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM social_graph
             WHERE follower=?1 AND followee=?2 AND state='active'",
        )
        .bind(me)
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    // ---- Followers ----

    pub async fn is_follower(&self, me: &str, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM social_graph
             WHERE follower=?1 AND followee=?2",
        )
        .bind(pubkey)
        .bind(me)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn upsert_follower(
        &self,
        me: &str,
        pubkey: &str,
        now: u64,
    ) -> Result<bool, AppError> {
        let existing: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM social_graph
             WHERE follower=?1 AND followee=?2",
        )
        .bind(pubkey)
        .bind(me)
        .fetch_one(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO social_graph (follower, followee, first_seen, last_seen, is_online)
             VALUES (?1, ?2, ?3, ?3, 1)
             ON CONFLICT(follower, followee) DO UPDATE SET last_seen=?3, is_online=1",
        )
        .bind(pubkey)
        .bind(me)
        .bind(now as i64)
        .execute(&self.pool)
        .await?;
        Ok(!existing)
    }

    pub async fn set_follower_offline(&self, me: &str, pubkey: &str) -> Result<(), AppError> {
        sqlx::query(
            "UPDATE social_graph SET is_online=0
             WHERE follower=?1 AND followee=?2",
        )
        .bind(pubkey)
        .bind(me)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_followers(&self, me: &str) -> Result<Vec<SocialGraphEntry>, AppError> {
        let rows: Vec<(String, i64, i64, i32)> = sqlx::query_as(
            "SELECT follower, first_seen, last_seen, is_online FROM social_graph
             WHERE followee=?1 AND first_seen IS NOT NULL ORDER BY last_seen DESC",
        )
        .bind(me)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(pubkey, first_seen, last_seen, is_online)| SocialGraphEntry {
                    pubkey,
                    followed_at: 0,
                    first_seen: first_seen as u64,
                    last_seen: last_seen as u64,
                    is_online: is_online != 0,
                },
            )
            .collect())
    }

    pub async fn is_mutual(&self, me: &str, pubkey: &str) -> Result<bool, AppError> {
        let is_follower = self.is_follower(me, pubkey).await?;
        let is_following = self.is_following(me, pubkey).await?;
        Ok(is_follower && is_following)
    }

    // ---- Remote social cache (uses same table) ----

    pub async fn cache_remote_follows(
        &self,
        remote_pubkey: &str,
        follows: &[SocialGraphEntry],
        hidden: bool,
    ) -> Result<(), AppError> {
        let now = now_millis();
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM social_graph WHERE follower = ?1 AND followed_at IS NOT NULL")
            .bind(remote_pubkey)
            .execute(&mut *tx)
            .await?;

        for f in follows {
            sqlx::query(
                "INSERT INTO social_graph (follower, followee, followed_at, state, last_changed_at)
                 VALUES (?1, ?2, ?3, 'active', 0)
                 ON CONFLICT(follower, followee) DO UPDATE SET followed_at=?3, state='active'",
            )
            .bind(remote_pubkey)
            .bind(&f.pubkey)
            .bind(f.followed_at as i64)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "INSERT INTO remote_social_meta (pubkey, follows_hidden, follows_fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(pubkey) DO UPDATE SET follows_hidden = ?2, follows_fetched_at = ?3",
        )
        .bind(remote_pubkey)
        .bind(hidden)
        .bind(now as i64)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn cache_remote_followers(
        &self,
        remote_pubkey: &str,
        followers: &[SocialGraphEntry],
        hidden: bool,
    ) -> Result<(), AppError> {
        let now = now_millis();
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM social_graph WHERE followee = ?1 AND first_seen IS NOT NULL")
            .bind(remote_pubkey)
            .execute(&mut *tx)
            .await?;

        for f in followers {
            sqlx::query(
                "INSERT INTO social_graph (follower, followee, first_seen, last_seen, is_online)
                 VALUES (?1, ?2, ?3, 0, 0)
                 ON CONFLICT(follower, followee) DO UPDATE SET first_seen=?3",
            )
            .bind(&f.pubkey)
            .bind(remote_pubkey)
            .bind(f.first_seen as i64)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "INSERT INTO remote_social_meta (pubkey, followers_hidden, followers_fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(pubkey) DO UPDATE SET followers_hidden = ?2, followers_fetched_at = ?3",
        )
        .bind(remote_pubkey)
        .bind(hidden)
        .bind(now as i64)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn get_cached_remote_follows(
        &self,
        remote_pubkey: &str,
    ) -> Result<Option<CachedRemoteSocial>, AppError> {
        let meta: Option<(bool, Option<i64>)> = sqlx::query_as(
            "SELECT follows_hidden, follows_fetched_at FROM remote_social_meta WHERE pubkey = ?1",
        )
        .bind(remote_pubkey)
        .fetch_optional(&self.pool)
        .await?;

        let Some((hidden, fetched_at)) = meta else {
            return Ok(None);
        };

        let follows = self.get_follows(remote_pubkey).await?;

        Ok(Some(CachedRemoteSocial {
            entries: follows,
            hidden,
            fetched_at: fetched_at.map(|t| t as u64),
        }))
    }

    pub async fn get_cached_remote_followers(
        &self,
        remote_pubkey: &str,
    ) -> Result<Option<CachedRemoteSocial>, AppError> {
        let meta: Option<(bool, Option<i64>)> = sqlx::query_as(
            "SELECT followers_hidden, followers_fetched_at FROM remote_social_meta WHERE pubkey = ?1",
        )
        .bind(remote_pubkey)
        .fetch_optional(&self.pool)
        .await?;

        let Some((hidden, fetched_at)) = meta else {
            return Ok(None);
        };

        let followers = self.get_followers(remote_pubkey).await?;

        Ok(Some(CachedRemoteSocial {
            entries: followers,
            hidden,
            fetched_at: fetched_at.map(|t| t as u64),
        }))
    }

    #[allow(dead_code)]
    pub async fn clear_remote_social_cache(&self, remote_pubkey: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM social_graph WHERE follower = ?1 OR followee = ?1")
            .bind(remote_pubkey)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM remote_social_meta WHERE pubkey = ?1")
            .bind(remote_pubkey)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
