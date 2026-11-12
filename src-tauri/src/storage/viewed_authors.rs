use crate::error::AppError;

use super::Storage;

/// Non-followed authors whose synced content is kept.
pub const MAX_VIEWED_AUTHORS: i64 = 50;
/// Posts, and interactions, kept per non-followed author.
pub const MAX_VIEWED_AUTHOR_ITEMS: i64 = 2_000;

impl Storage {
    /// Record that a profile view synced the content of `author`, whom the local user does
    /// not follow, at `now`. Keeps the newest `MAX_VIEWED_AUTHORS` records and returns the
    /// authors whose records were dropped; the caller deletes their content.
    pub async fn record_viewed_author(
        &self,
        author: &str,
        now: u64,
    ) -> Result<Vec<String>, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO viewed_authors (pubkey, synced_at) VALUES (?1, ?2)
             ON CONFLICT(pubkey) DO UPDATE SET synced_at = excluded.synced_at",
        )
        .bind(author)
        .bind(i64::try_from(now).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await?;
        let dropped: Vec<String> = sqlx::query_scalar(
            "DELETE FROM viewed_authors WHERE pubkey IN (
                 SELECT pubkey FROM viewed_authors ORDER BY synced_at DESC, pubkey
                 LIMIT -1 OFFSET ?1
             ) RETURNING pubkey",
        )
        .bind(MAX_VIEWED_AUTHORS)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(dropped)
    }

    /// The recorded authors.
    pub async fn get_viewed_authors(&self) -> Result<Vec<String>, AppError> {
        let authors = sqlx::query_scalar("SELECT pubkey FROM viewed_authors")
            .fetch_all(&self.pool)
            .await?;
        Ok(authors)
    }

    /// Delete the records of the authors synced before `cutoff` and return them; the
    /// caller deletes their content.
    pub async fn take_viewed_authors_before(&self, cutoff: u64) -> Result<Vec<String>, AppError> {
        let authors =
            sqlx::query_scalar("DELETE FROM viewed_authors WHERE synced_at < ?1 RETURNING pubkey")
                .bind(i64::try_from(cutoff).unwrap_or(i64::MAX))
                .fetch_all(&self.pool)
                .await?;
        Ok(authors)
    }

    /// Keep the newest `MAX_VIEWED_AUTHOR_ITEMS` posts and the newest
    /// `MAX_VIEWED_AUTHOR_ITEMS` interactions of `author` and delete the others, with the
    /// notifications about the deleted posts. Returns the number of posts deleted.
    pub async fn trim_author_content(&self, author: &str) -> Result<u64, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        const EXCESS_POSTS: &str = "SELECT id FROM posts WHERE author=?1
             ORDER BY timestamp DESC, id DESC LIMIT -1 OFFSET ?2";
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM notifications
             WHERE post_id IN ({EXCESS_POSTS}) OR target_post_id IN ({EXCESS_POSTS})"
        )))
        .bind(author)
        .bind(MAX_VIEWED_AUTHOR_ITEMS)
        .execute(&mut *tx)
        .await?;
        let posts = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM posts WHERE author=?1 AND id IN ({EXCESS_POSTS})"
        )))
        .bind(author)
        .bind(MAX_VIEWED_AUTHOR_ITEMS)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        sqlx::query(
            "DELETE FROM interactions WHERE author=?1 AND id IN (
                 SELECT id FROM interactions WHERE author=?1
                 ORDER BY timestamp DESC, id DESC LIMIT -1 OFFSET ?2
             )",
        )
        .bind(author)
        .bind(MAX_VIEWED_AUTHOR_ITEMS)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(posts)
    }

    /// Delete the posts, with their notifications, and the interactions of `author` unless
    /// `me` follows `author`. Returns the number of posts deleted.
    pub async fn delete_content_unless_followed(
        &self,
        me: &str,
        author: &str,
    ) -> Result<u64, AppError> {
        if author == me || self.is_following(me, author).await? {
            return Ok(0);
        }
        let posts = self.delete_posts_by_author(author).await?;
        sqlx::query("DELETE FROM interactions WHERE author=?1")
            .bind(author)
            .execute(&self.pool)
            .await?;
        Ok(posts)
    }
}
