use crate::error::AppError;
use proscenium_types::{MediaAttachment, Post};
use sqlx::Row;

use super::{Cursor, Storage, page_limit, sql_timestamp};

/// Column list matching the indexes read by `row_to_post`.
const POST_COLUMNS: &str = "id, author, content, timestamp, media_json, reply_to, reply_to_author, quote_of, quote_of_author, signature";

/// Predicate that excludes posts of muted and blocked authors.
const AUTHOR_NOT_HIDDEN: &str =
    "author NOT IN (SELECT pubkey FROM moderation WHERE state='active')";

/// Predicate that selects a repost: a post with `quote_of` set, no text and no media.
/// A post with `quote_of` and text or media is a quote.
pub(super) const IS_PURE_REPOST: &str = "content='' AND media_json='[]'";

impl Storage {
    fn row_to_post(row: &sqlx::sqlite::SqliteRow) -> Result<Post, AppError> {
        let media_json: String = row.get(4);
        let media: Vec<MediaAttachment> = serde_json::from_str(&media_json)?;
        Ok(Post {
            id: row.get(0),
            author: row.get(1),
            content: row.get(2),
            timestamp: row.get::<i64, _>(3) as u64,
            media,
            reply_to: row.get(5),
            reply_to_author: row.get(6),
            quote_of: row.get(7),
            quote_of_author: row.get(8),
            signature: row.get(9),
        })
    }

    fn rows_to_posts(rows: &[sqlx::sqlite::SqliteRow]) -> Result<Vec<Post>, AppError> {
        rows.iter().map(Self::row_to_post).collect()
    }

    /// Insert a post. Returns `false` if a row with the same `(author, id)` already exists.
    pub async fn insert_post(&self, post: &Post) -> Result<bool, AppError> {
        let media_json = serde_json::to_string(&post.media)?;
        let result = sqlx::query(
            "INSERT OR IGNORE INTO posts (id, author, content, timestamp, media_json, reply_to, reply_to_author, quote_of, quote_of_author, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(&post.id)
        .bind(&post.author)
        .bind(&post.content)
        .bind(post.timestamp as i64)
        .bind(&media_json)
        .bind(&post.reply_to)
        .bind(&post.reply_to_author)
        .bind(&post.quote_of)
        .bind(&post.quote_of_author)
        .bind(&post.signature)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Return the post with this id.
    ///
    /// `validate_post` requires the id to be a hash of the post content including the author,
    /// so at most one validated post has a given id. If rows of several authors share an id,
    /// the row with the smallest author key is returned.
    pub async fn get_post_by_id(&self, id: &str) -> Result<Option<Post>, AppError> {
        let sql = format!("SELECT {POST_COLUMNS} FROM posts WHERE id=?1 ORDER BY author LIMIT 1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(Self::row_to_post).transpose()
    }

    /// Home feed of `viewer`: posts by `viewer` and by the authors `viewer` follows,
    /// without muted and blocked authors, ordered by `(timestamp, id)` descending.
    /// `before` selects the rows after that position in this order.
    pub async fn get_feed(
        &self,
        viewer: &str,
        limit: usize,
        before: Option<Cursor<'_>>,
    ) -> Result<Vec<Post>, AppError> {
        let mut sql = format!(
            "SELECT {POST_COLUMNS} FROM posts
             WHERE (author = ?1 OR EXISTS (
                        SELECT 1 FROM social_graph s
                        WHERE s.follower = ?1 AND s.followee = posts.author AND s.state = 'active'))
               AND {AUTHOR_NOT_HIDDEN}"
        );
        if before.is_some() {
            sql.push_str(" AND (timestamp, id) < (?3, ?4)");
        }
        sql.push_str(" ORDER BY timestamp DESC, id DESC LIMIT ?2");

        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(viewer)
            .bind(page_limit(limit));
        if let Some((timestamp, id)) = before {
            query = query.bind(sql_timestamp(timestamp)).bind(id);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Self::rows_to_posts(&rows)
    }

    /// Remove the rows that refer to post `id`: interactions that target it and its
    /// notifications are deleted; its bookmark is set to `state='removed'`, because device
    /// sync merges bookmark rows by `last_changed_at` and needs the row to carry the removal.
    async fn delete_post_dependents(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: &str,
    ) -> Result<(), AppError> {
        sqlx::query("DELETE FROM interactions WHERE target_post_id=?1")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "UPDATE bookmarks SET state='removed', last_changed_at=?2
             WHERE post_id=?1 AND state='active'",
        )
        .bind(id)
        .bind(proscenium_types::now_millis() as i64)
        .execute(&mut **tx)
        .await?;
        sqlx::query("DELETE FROM notifications WHERE post_id=?1 OR target_post_id=?1")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Delete the post `(author, id)` together with the interactions that target it, its
    /// bookmark and its notifications, in one transaction.
    ///
    /// Returns `false` and deletes nothing if `author` has no post with this id, so a
    /// delete issued by one author cannot remove rows that refer to another author's post.
    pub async fn delete_post(&self, author: &str, id: &str) -> Result<bool, AppError> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query("DELETE FROM posts WHERE author=?1 AND id=?2")
            .bind(author)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            return Ok(false);
        }
        Self::delete_post_dependents(&mut tx, id).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Delete every stored post of `author` and the notifications that refer to those
    /// posts, in one transaction. Returns the number of posts deleted.
    ///
    /// Interactions and bookmarks are kept: this removes the local copies of posts that
    /// still exist on their author's devices.
    pub async fn delete_posts_by_author(&self, author: &str) -> Result<u64, AppError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM notifications WHERE post_id IN (SELECT id FROM posts WHERE author=?1) OR target_post_id IN (SELECT id FROM posts WHERE author=?1)",
        )
        .bind(author)
        .execute(&mut *tx)
        .await?;
        let result = sqlx::query("DELETE FROM posts WHERE author=?1")
            .bind(author)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }

    /// True if `author` has a repost (no text, no media) of `target_post_id`.
    pub async fn has_repost(&self, author: &str, target_post_id: &str) -> Result<bool, AppError> {
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM posts WHERE author=?1 AND quote_of=?2 AND {IS_PURE_REPOST})"
        );
        let exists: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(author)
            .bind(target_post_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(exists)
    }

    /// Delete every repost (no text, no media) of `target_post_id` by `author`, with the
    /// rows that refer to each deleted post, in one transaction. Quotes are kept.
    /// Returns the ids of the deleted posts.
    pub async fn delete_repost_by_target(
        &self,
        author: &str,
        target_post_id: &str,
    ) -> Result<Vec<String>, AppError> {
        let mut tx = self.pool.begin().await?;
        let sql = format!(
            "DELETE FROM posts WHERE author=?1 AND quote_of=?2 AND {IS_PURE_REPOST} RETURNING id"
        );
        let ids: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(author)
            .bind(target_post_id)
            .fetch_all(&mut *tx)
            .await?;
        for id in &ids {
            Self::delete_post_dependents(&mut tx, id).await?;
        }
        tx.commit().await?;
        Ok(ids)
    }

    /// Posts of `author` ordered by `(timestamp, id)` descending.
    /// `before` selects the rows after that position in this order.
    pub async fn get_posts_by_author(
        &self,
        author: &str,
        limit: usize,
        before: Option<Cursor<'_>>,
        media_filter: Option<&str>,
    ) -> Result<Vec<Post>, AppError> {
        let filter_clause = match media_filter {
            Some("images") => " AND media_json LIKE '%image/%'",
            Some("videos") => " AND media_json LIKE '%video/%'",
            Some("audio") => " AND media_json LIKE '%audio/%'",
            Some("files") => {
                " AND media_json != '[]' AND media_json NOT LIKE '%image/%' AND media_json NOT LIKE '%video/%' AND media_json NOT LIKE '%audio/%'"
            }
            Some("text") => " AND media_json = '[]'",
            _ => "",
        };

        let mut sql = format!("SELECT {POST_COLUMNS} FROM posts WHERE author=?1{filter_clause}");
        if before.is_some() {
            sql.push_str(" AND (timestamp, id) < (?3, ?4)");
        }
        sql.push_str(" ORDER BY timestamp DESC, id DESC LIMIT ?2");

        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(author)
            .bind(page_limit(limit));
        if let Some((timestamp, id)) = before {
            query = query.bind(sql_timestamp(timestamp)).bind(id);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Self::rows_to_posts(&rows)
    }

    pub async fn get_post_ids_by_author(&self, author: &str) -> Result<Vec<String>, AppError> {
        let rows =
            sqlx::query("SELECT id FROM posts WHERE author=?1 ORDER BY timestamp ASC, id ASC")
                .bind(author)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn count_posts_by_author(&self, author: &str) -> Result<u64, AppError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE author=?1")
            .bind(author)
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u64)
    }

    pub async fn newest_post_timestamp(&self, author: &str) -> Result<u64, AppError> {
        let ts: Option<i64> =
            sqlx::query_scalar("SELECT MAX(timestamp) FROM posts WHERE author=?1")
                .bind(author)
                .fetch_one(&self.pool)
                .await?;
        Ok(ts.unwrap_or(0) as u64)
    }

    pub async fn count_posts_after(&self, author: &str, after_ts: u64) -> Result<u64, AppError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE author=?1 AND timestamp > ?2")
                .bind(author)
                .bind(sql_timestamp(after_ts))
                .fetch_one(&self.pool)
                .await?;
        Ok(count as u64)
    }

    /// Replies to `parent_post_id` without muted and blocked authors, ordered by
    /// `(timestamp, id)` ascending. `after` selects the rows after that position.
    pub async fn get_replies(
        &self,
        parent_post_id: &str,
        limit: usize,
        after: Option<Cursor<'_>>,
    ) -> Result<Vec<Post>, AppError> {
        let mut sql =
            format!("SELECT {POST_COLUMNS} FROM posts WHERE reply_to=?1 AND {AUTHOR_NOT_HIDDEN}");
        if after.is_some() {
            sql.push_str(" AND (timestamp, id) > (?3, ?4)");
        }
        sql.push_str(" ORDER BY timestamp ASC, id ASC LIMIT ?2");

        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(parent_post_id)
            .bind(page_limit(limit));
        if let Some((timestamp, id)) = after {
            query = query.bind(sql_timestamp(timestamp)).bind(id);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Self::rows_to_posts(&rows)
    }
}
