use rusqlite::params;

use super::Storage;

const PUSH_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000; // 7 days

impl Storage {
    /// Enqueue a profile-only push (both post_id and interaction_id NULL).
    pub fn enqueue_push_profile(&self, recipient: &str) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis();
        let expires_at = now + PUSH_TTL_MS;
        self.with_db(|db| {
            db.execute(
                "INSERT INTO push_outbox (recipient, created_at, expires_at) VALUES (?1, ?2, ?3)",
                params![recipient, now as i64, expires_at as i64],
            )?;
            Ok(())
        })
    }

    pub fn enqueue_push_post(&self, recipient: &str, post_id: &str) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis();
        let expires_at = now + PUSH_TTL_MS;
        self.with_db(|db| {
            db.execute(
                "INSERT INTO push_outbox (recipient, post_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
                params![recipient, post_id, now as i64, expires_at as i64],
            )?;
            Ok(())
        })
    }

    pub fn enqueue_push_interaction(
        &self,
        recipient: &str,
        interaction_id: &str,
    ) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis();
        let expires_at = now + PUSH_TTL_MS;
        self.with_db(|db| {
            db.execute(
                "INSERT INTO push_outbox (recipient, interaction_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
                params![recipient, interaction_id, now as i64, expires_at as i64],
            )?;
            Ok(())
        })
    }

    pub fn get_push_outbox_peers(&self) -> anyhow::Result<Vec<String>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT DISTINCT recipient FROM push_outbox WHERE attempts < max_attempts ORDER BY created_at ASC",
            )?;
            let mut rows = stmt.query([])?;
            let mut peers = Vec::new();
            while let Some(row) = rows.next()? {
                peers.push(row.get(0)?);
            }
            Ok(peers)
        })
    }

    /// Get profile-only push entries (both post_id and interaction_id NULL).
    pub fn get_pending_push_profile_ids(&self, recipient: &str) -> anyhow::Result<Vec<i64>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT id FROM push_outbox WHERE recipient=?1 AND post_id IS NULL AND interaction_id IS NULL AND attempts < max_attempts",
            )?;
            let mut rows = stmt.query(params![recipient])?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next()? {
                ids.push(row.get(0)?);
            }
            Ok(ids)
        })
    }

    pub fn get_pending_push_post_ids(&self, recipient: &str) -> anyhow::Result<Vec<(i64, String)>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT id, post_id FROM push_outbox WHERE recipient=?1 AND post_id IS NOT NULL AND attempts < max_attempts ORDER BY created_at ASC LIMIT 50",
            )?;
            let mut rows = stmt.query(params![recipient])?;
            let mut items = Vec::new();
            while let Some(row) = rows.next()? {
                items.push((row.get(0)?, row.get(1)?));
            }
            Ok(items)
        })
    }

    pub fn get_pending_push_interaction_ids(
        &self,
        recipient: &str,
    ) -> anyhow::Result<Vec<(i64, String)>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT id, interaction_id FROM push_outbox WHERE recipient=?1 AND interaction_id IS NOT NULL AND attempts < max_attempts ORDER BY created_at ASC LIMIT 200",
            )?;
            let mut rows = stmt.query(params![recipient])?;
            let mut items = Vec::new();
            while let Some(row) = rows.next()? {
                items.push((row.get(0)?, row.get(1)?));
            }
            Ok(items)
        })
    }

    pub fn mark_push_attempted(&self, outbox_ids: &[i64]) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis();
        self.with_db(|db| {
            for id in outbox_ids {
                db.execute(
                    "UPDATE push_outbox SET attempts = attempts + 1, last_attempt_at = ?2 WHERE id = ?1",
                    params![id, now as i64],
                )?;
            }
            Ok(())
        })
    }

    pub fn remove_push_outbox_entries(&self, outbox_ids: &[i64]) -> anyhow::Result<()> {
        self.with_db(|db| {
            for id in outbox_ids {
                db.execute("DELETE FROM push_outbox WHERE id = ?1", params![id])?;
            }
            Ok(())
        })
    }

    pub fn prune_expired_push_entries(&self) -> anyhow::Result<u64> {
        let now = iroh_social_types::now_millis();
        self.with_db(|db| {
            let count = db.execute(
                "DELETE FROM push_outbox WHERE expires_at < ?1 OR attempts >= max_attempts",
                params![now as i64],
            )?;
            Ok(count as u64)
        })
    }
}
