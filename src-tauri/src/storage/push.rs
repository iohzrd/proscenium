use sqlx::Row;

use super::Storage;

const PUSH_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000; // 7 days

impl Storage {
    pub async fn enqueue_push_profile(&self, recipient: &str) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis();
        let expires_at = now + PUSH_TTL_MS;
        sqlx::query(
            "INSERT INTO push_outbox (recipient, created_at, expires_at) VALUES (?1, ?2, ?3)",
        )
        .bind(recipient)
        .bind(now as i64)
        .bind(expires_at as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn enqueue_push_post(&self, recipient: &str, post_id: &str) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis();
        let expires_at = now + PUSH_TTL_MS;
        sqlx::query(
            "INSERT INTO push_outbox (recipient, post_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(recipient)
        .bind(post_id)
        .bind(now as i64)
        .bind(expires_at as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn enqueue_push_interaction(
        &self,
        recipient: &str,
        interaction_id: &str,
    ) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis();
        let expires_at = now + PUSH_TTL_MS;
        sqlx::query(
            "INSERT INTO push_outbox (recipient, interaction_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(recipient)
        .bind(interaction_id)
        .bind(now as i64)
        .bind(expires_at as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_push_outbox_peers(&self) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT DISTINCT recipient FROM push_outbox WHERE attempts < max_attempts ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn get_pending_push_profile_ids(&self, recipient: &str) -> anyhow::Result<Vec<i64>> {
        let rows = sqlx::query(
            "SELECT id FROM push_outbox WHERE recipient=?1 AND post_id IS NULL AND interaction_id IS NULL AND attempts < max_attempts",
        )
        .bind(recipient)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn get_pending_push_post_ids(
        &self,
        recipient: &str,
    ) -> anyhow::Result<Vec<(i64, String)>> {
        let rows = sqlx::query(
            "SELECT id, post_id FROM push_outbox WHERE recipient=?1 AND post_id IS NOT NULL AND attempts < max_attempts ORDER BY created_at ASC LIMIT 50",
        )
        .bind(recipient)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    pub async fn get_pending_push_interaction_ids(
        &self,
        recipient: &str,
    ) -> anyhow::Result<Vec<(i64, String)>> {
        let rows = sqlx::query(
            "SELECT id, interaction_id FROM push_outbox WHERE recipient=?1 AND interaction_id IS NOT NULL AND attempts < max_attempts ORDER BY created_at ASC LIMIT 200",
        )
        .bind(recipient)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    pub async fn mark_push_attempted(&self, outbox_ids: &[i64]) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis();
        for id in outbox_ids {
            sqlx::query(
                "UPDATE push_outbox SET attempts = attempts + 1, last_attempt_at = ?2 WHERE id = ?1",
            )
            .bind(id)
            .bind(now as i64)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn remove_push_outbox_entries(&self, outbox_ids: &[i64]) -> anyhow::Result<()> {
        for id in outbox_ids {
            sqlx::query("DELETE FROM push_outbox WHERE id = ?1")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn prune_expired_push_entries(&self) -> anyhow::Result<u64> {
        let now = iroh_social_types::now_millis();
        let result = sqlx::query(
            "DELETE FROM push_outbox WHERE expires_at < ?1 OR attempts >= max_attempts",
        )
        .bind(now as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
