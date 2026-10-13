use crate::error::AppError;
use proscenium_types::{Interaction, InteractionKind, PostCounts};
use sqlx::Row;

use super::posts::IS_PURE_REPOST;
use super::{Storage, page_limit, page_offset, sql_timestamp};

const INTERACTION_COLUMNS: &str =
    "id, author, kind, target_post_id, target_author, timestamp, signature";

impl Storage {
    fn row_to_interaction(row: &sqlx::sqlite::SqliteRow) -> Result<Interaction, AppError> {
        let kind_str: String = row.get(2);
        let kind = match kind_str.to_lowercase().as_str() {
            "like" => InteractionKind::Like,
            other => {
                return Err(AppError::Other(format!(
                    "unknown interaction kind: {other}"
                )));
            }
        };
        Ok(Interaction {
            id: row.get(0),
            author: row.get(1),
            kind,
            target_post_id: row.get(3),
            target_author: row.get(4),
            timestamp: row.get::<i64, _>(5) as u64,
            signature: row.get(6),
        })
    }

    /// Insert an interaction. Returns `false` if a row with the same `(author, id)` or the
    /// same `(author, kind, target_post_id)` is already stored.
    pub async fn save_interaction(&self, interaction: &Interaction) -> Result<bool, AppError> {
        let kind_str = match interaction.kind {
            InteractionKind::Like => "Like",
        };
        let result = sqlx::query(
            "INSERT OR IGNORE INTO interactions (id, author, kind, target_post_id, target_author, timestamp, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&interaction.id)
        .bind(&interaction.author)
        .bind(kind_str)
        .bind(&interaction.target_post_id)
        .bind(&interaction.target_author)
        .bind(interaction.timestamp as i64)
        .bind(&interaction.signature)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Return the interaction of `author` with the given kind that targets `target_post_id`.
    /// The unique index on `(author, kind, target_post_id)` allows at most one.
    pub async fn get_interaction_by_target(
        &self,
        author: &str,
        kind: &str,
        target_post_id: &str,
    ) -> Result<Option<Interaction>, AppError> {
        let sql = format!(
            "SELECT {INTERACTION_COLUMNS} FROM interactions
             WHERE author=?1 AND kind=?2 AND target_post_id=?3"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(author)
            .bind(kind)
            .bind(target_post_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(Self::row_to_interaction).transpose()
    }

    /// Delete the "like" notification that `actor`'s like of `target_post_id` created.
    async fn delete_like_notification(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        actor: &str,
        target_post_id: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "DELETE FROM notifications WHERE kind='like' AND actor=?1 AND target_post_id=?2",
        )
        .bind(actor)
        .bind(target_post_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Delete the interaction `(author, id)` and the "like" notification it created, in one
    /// transaction. Returns `false` if no such interaction is stored.
    pub async fn delete_interaction(&self, id: &str, author: &str) -> Result<bool, AppError> {
        let mut tx = self.pool.begin().await?;
        let target: Option<String> = sqlx::query_scalar(
            "DELETE FROM interactions WHERE id=?1 AND author=?2 RETURNING target_post_id",
        )
        .bind(id)
        .bind(author)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(target_post_id) = target else {
            return Ok(false);
        };
        Self::delete_like_notification(&mut tx, author, &target_post_id).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Delete the interaction of `author` with the given kind that targets `target_post_id`,
    /// and the "like" notification it created, in one transaction.
    /// Returns the id of the deleted interaction.
    pub async fn delete_interaction_by_target(
        &self,
        author: &str,
        kind: &str,
        target_post_id: &str,
    ) -> Result<Option<String>, AppError> {
        let mut tx = self.pool.begin().await?;
        let id: Option<String> = sqlx::query_scalar(
            "DELETE FROM interactions WHERE author=?1 AND kind=?2 AND target_post_id=?3 RETURNING id",
        )
        .bind(author)
        .bind(kind)
        .bind(target_post_id)
        .fetch_optional(&mut *tx)
        .await?;
        if id.is_some() {
            Self::delete_like_notification(&mut tx, author, target_post_id).await?;
            tx.commit().await?;
        }
        Ok(id)
    }

    /// Counts for the post `target_post_id` as seen by `my_pubkey`.
    ///
    /// `reposts` counts every post that has `quote_of` set to the target (reposts and
    /// quotes). `reposted_by_me` is true only if `my_pubkey` has a repost without text and
    /// media; a quote does not set it.
    pub async fn get_post_counts(
        &self,
        my_pubkey: &str,
        target_post_id: &str,
    ) -> Result<PostCounts, AppError> {
        let sql = format!(
            "SELECT
                (SELECT COUNT(*) FROM interactions WHERE target_post_id=?1 AND kind='Like') AS likes,
                (SELECT COUNT(*) FROM posts WHERE reply_to=?1) AS replies,
                (SELECT COUNT(*) FROM posts WHERE quote_of=?1) AS reposts,
                EXISTS(SELECT 1 FROM interactions WHERE author=?2 AND kind='Like' AND target_post_id=?1) AS liked_by_me,
                EXISTS(SELECT 1 FROM posts WHERE author=?2 AND quote_of=?1 AND {IS_PURE_REPOST}) AS reposted_by_me"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(target_post_id)
            .bind(my_pubkey)
            .fetch_one(&self.pool)
            .await?;
        Ok(PostCounts {
            likes: row.get::<i64, _>(0) as u32,
            replies: row.get::<i64, _>(1) as u32,
            reposts: row.get::<i64, _>(2) as u32,
            liked_by_me: row.get::<bool, _>(3),
            reposted_by_me: row.get::<bool, _>(4),
        })
    }

    pub async fn count_interactions_by_author(&self, author: &str) -> Result<u64, AppError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM interactions WHERE author=?1")
            .bind(author)
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u64)
    }

    pub async fn newest_interaction_timestamp(&self, author: &str) -> Result<u64, AppError> {
        let ts: Option<i64> =
            sqlx::query_scalar("SELECT MAX(timestamp) FROM interactions WHERE author=?1")
                .bind(author)
                .fetch_one(&self.pool)
                .await?;
        Ok(ts.unwrap_or(0) as u64)
    }

    pub async fn count_interactions_after(
        &self,
        author: &str,
        after_ts: u64,
    ) -> Result<u64, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM interactions WHERE author=?1 AND timestamp > ?2",
        )
        .bind(author)
        .bind(sql_timestamp(after_ts))
        .fetch_one(&self.pool)
        .await?;
        Ok(count as u64)
    }

    /// Interactions of `author` with `timestamp > after_ts`, ordered by `(timestamp, id)`
    /// ascending. At most `MAX_PAGE_SIZE` rows are returned per call.
    pub async fn get_interactions_after(
        &self,
        author: &str,
        after_ts: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Interaction>, AppError> {
        let sql = format!(
            "SELECT {INTERACTION_COLUMNS} FROM interactions WHERE author=?1 AND timestamp > ?2
             ORDER BY timestamp ASC, id ASC LIMIT ?3 OFFSET ?4"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(author)
            .bind(sql_timestamp(after_ts))
            .bind(page_limit(limit))
            .bind(page_offset(offset))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(Self::row_to_interaction).collect()
    }

    /// Interactions of `author` ordered by `(timestamp, id)` ascending.
    /// At most `MAX_PAGE_SIZE` rows are returned per call.
    pub async fn get_interactions_paged(
        &self,
        author: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Interaction>, AppError> {
        let sql = format!(
            "SELECT {INTERACTION_COLUMNS} FROM interactions WHERE author=?1
             ORDER BY timestamp ASC, id ASC LIMIT ?2 OFFSET ?3"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(author)
            .bind(page_limit(limit))
            .bind(page_offset(offset))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(Self::row_to_interaction).collect()
    }
}
