use crate::storage::Storage;
use iroh_social_types::{IdentityResponse, SigningKeyDelegation, now_millis};

impl Storage {
    pub async fn cache_peer_identity(&self, response: &IdentityResponse) -> anyhow::Result<()> {
        let delegation_json = serde_json::to_string(&response.delegation)?;
        let transport_json = serde_json::to_string(&response.transport_node_ids)?;
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO peer_delegations (master_pubkey, signing_pubkey, dm_pubkey, delegation_json, transport_node_ids_json, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(master_pubkey) DO UPDATE SET
                signing_pubkey = ?2,
                dm_pubkey = ?3,
                delegation_json = ?4,
                transport_node_ids_json = ?5,
                cached_at = ?6",
        )
        .bind(&response.master_pubkey)
        .bind(&response.delegation.signing_pubkey)
        .bind(&response.delegation.dm_pubkey)
        .bind(&delegation_json)
        .bind(&transport_json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn get_peer_delegation(
        &self,
        master_pubkey: &str,
    ) -> anyhow::Result<Option<SigningKeyDelegation>> {
        let result: Option<String> = sqlx::query_scalar(
            "SELECT delegation_json FROM peer_delegations WHERE master_pubkey = ?1",
        )
        .bind(master_pubkey)
        .fetch_optional(&self.pool)
        .await?;
        match result {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    pub async fn get_peer_transport_node_ids(
        &self,
        master_pubkey: &str,
    ) -> anyhow::Result<Vec<String>> {
        let result: Option<String> = sqlx::query_scalar(
            "SELECT transport_node_ids_json FROM peer_delegations WHERE master_pubkey = ?1",
        )
        .bind(master_pubkey)
        .fetch_optional(&self.pool)
        .await?;
        match result {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(vec![]),
        }
    }

    pub async fn get_master_pubkey_for_dm_pubkey(&self, dm_pubkey: &str) -> Option<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT master_pubkey FROM peer_delegations WHERE dm_pubkey = ?1",
        )
        .bind(dm_pubkey)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
    }

    pub async fn get_master_pubkey_for_transport(&self, transport_id: &str) -> Option<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT p.master_pubkey FROM peer_delegations p, json_each(p.transport_node_ids_json) j
             WHERE j.value = ?1
             LIMIT 1",
        )
        .bind(transport_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
    }

    pub async fn get_peer_signing_pubkey(
        &self,
        master_pubkey: &str,
    ) -> anyhow::Result<Option<String>> {
        let result: Option<String> = sqlx::query_scalar(
            "SELECT signing_pubkey FROM peer_delegations WHERE master_pubkey = ?1",
        )
        .bind(master_pubkey)
        .fetch_optional(&self.pool)
        .await?;
        Ok(result)
    }

    pub async fn get_peer_dm_pubkey(&self, master_pubkey: &str) -> anyhow::Result<Option<String>> {
        let result: Option<String> =
            sqlx::query_scalar("SELECT dm_pubkey FROM peer_delegations WHERE master_pubkey = ?1")
                .bind(master_pubkey)
                .fetch_optional(&self.pool)
                .await?;
        match result {
            Some(pubkey) if !pubkey.is_empty() => Ok(Some(pubkey)),
            _ => Ok(None),
        }
    }
}
