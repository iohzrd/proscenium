use iroh_social_types::{Interaction, InteractionKind};
use sqlx::Row;

use super::{PostCounts, Storage};

impl Storage {
    fn row_to_interaction(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Interaction> {
        let kind_str: String = row.get(2);
        let kind = match kind_str.to_lowercase().as_str() {
            "like" => InteractionKind::Like,
            other => anyhow::bail!("unknown interaction kind: {other}"),
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

    pub async fn get_interaction_by_id(&self, id: &str) -> anyhow::Result<Option<Interaction>> {
        let row = sqlx::query(
            "SELECT id, author, kind, target_post_id, target_author, timestamp, signature FROM interactions WHERE id=?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => Ok(Some(Self::row_to_interaction(&row)?)),
            None => Ok(None),
        }
    }

    pub async fn save_interaction(&self, interaction: &Interaction) -> anyhow::Result<()> {
        let kind_str = match interaction.kind {
            InteractionKind::Like => "Like",
        };
        sqlx::query(
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
        Ok(())
    }

    pub async fn delete_interaction(&self, id: &str, author: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM interactions WHERE id=?1 AND author=?2")
            .bind(id)
            .bind(author)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_interaction_by_target(
        &self,
        author: &str,
        kind: &str,
        target_post_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM interactions WHERE author=?1 AND kind=?2 AND target_post_id=?3",
        )
        .bind(author)
        .bind(kind)
        .bind(target_post_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(ref id) = id {
            sqlx::query("DELETE FROM interactions WHERE id=?1 AND author=?2")
                .bind(id)
                .bind(author)
                .execute(&self.pool)
                .await?;
        }
        Ok(id)
    }

    pub async fn get_post_counts(
        &self,
        my_pubkey: &str,
        target_post_id: &str,
    ) -> anyhow::Result<PostCounts> {
        let likes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM interactions WHERE target_post_id=?1 AND kind='Like'",
        )
        .bind(target_post_id)
        .fetch_one(&self.pool)
        .await?;
        let reposts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE quote_of=?1")
            .bind(target_post_id)
            .fetch_one(&self.pool)
            .await?;
        let replies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE reply_to=?1")
            .bind(target_post_id)
            .fetch_one(&self.pool)
            .await?;
        let liked_by_me: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM interactions WHERE author=?1 AND kind='Like' AND target_post_id=?2",
        )
        .bind(my_pubkey)
        .bind(target_post_id)
        .fetch_one(&self.pool)
        .await?;
        let reposted_by_me: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM posts WHERE author=?1 AND quote_of=?2")
                .bind(my_pubkey)
                .bind(target_post_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(PostCounts {
            likes: likes as u32,
            replies: replies as u32,
            reposts: reposts as u32,
            liked_by_me,
            reposted_by_me,
        })
    }

    pub async fn count_interactions_by_author(&self, author: &str) -> anyhow::Result<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM interactions WHERE author=?1")
            .bind(author)
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u64)
    }

    pub async fn newest_interaction_timestamp(&self, author: &str) -> anyhow::Result<u64> {
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
    ) -> anyhow::Result<u64> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM interactions WHERE author=?1 AND timestamp > ?2",
        )
        .bind(author)
        .bind(after_ts as i64)
        .fetch_one(&self.pool)
        .await?;
        Ok(count as u64)
    }

    pub async fn get_interactions_after(
        &self,
        author: &str,
        after_ts: u64,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<Interaction>> {
        let rows = sqlx::query(
            "SELECT id, author, kind, target_post_id, target_author, timestamp, signature
             FROM interactions WHERE author=?1 AND timestamp > ?2
             ORDER BY timestamp ASC LIMIT ?3 OFFSET ?4",
        )
        .bind(author)
        .bind(after_ts as i64)
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut interactions = Vec::new();
        for row in &rows {
            interactions.push(Self::row_to_interaction(row)?);
        }
        Ok(interactions)
    }

    pub async fn get_interactions_paged(
        &self,
        author: &str,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<Interaction>> {
        let rows = sqlx::query(
            "SELECT id, author, kind, target_post_id, target_author, timestamp, signature
             FROM interactions WHERE author=?1
             ORDER BY timestamp ASC LIMIT ?2 OFFSET ?3",
        )
        .bind(author)
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut interactions = Vec::new();
        for row in &rows {
            interactions.push(Self::row_to_interaction(row)?);
        }
        Ok(interactions)
    }
}
