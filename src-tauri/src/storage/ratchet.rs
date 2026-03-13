use super::Storage;

impl Storage {
    pub async fn save_ratchet_session(
        &self,
        peer_pubkey: &str,
        state_json: &str,
        updated_at: u64,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO dm_ratchet_sessions (peer_pubkey, state_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(peer_pubkey) DO UPDATE SET state_json=?2, updated_at=?3",
        )
        .bind(peer_pubkey)
        .bind(state_json)
        .bind(updated_at as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_ratchet_session(&self, peer_pubkey: &str) -> anyhow::Result<Option<String>> {
        let result: Option<String> =
            sqlx::query_scalar("SELECT state_json FROM dm_ratchet_sessions WHERE peer_pubkey=?1")
                .bind(peer_pubkey)
                .fetch_optional(&self.pool)
                .await?;
        Ok(result)
    }
}
