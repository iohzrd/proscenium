use crate::error::AppError;
use crate::storage::Storage;
use proscenium_types::{
    LinkedDevicesAnnouncement, SigningKeyDelegation, VerifiedIdentity, now_millis,
};

/// Result of offering a peer's device announcement to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnouncementOutcome {
    /// The device list and transport NodeIds were replaced.
    Stored,
    /// The cached announcement has an equal or higher version; the device list was kept.
    NotNewer,
}

impl Storage {
    /// Store a delegation whose master-key signature the caller has verified.
    ///
    /// Returns an error if the cached delegation for the same master pubkey has a higher
    /// `key_index` or `dm_key_index`, so a superseded delegation cannot replace a newer one.
    /// The transport NodeId list of an existing row is kept.
    pub async fn cache_peer_delegation(
        &self,
        delegation: &SigningKeyDelegation,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        Self::cache_peer_delegation_tx(&mut tx, delegation).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn cache_peer_delegation_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        delegation: &SigningKeyDelegation,
    ) -> Result<(), AppError> {
        let delegation_json = serde_json::to_string(delegation)?;
        let now = now_millis() as i64;
        // The first statement of the transaction is a write, so no other writer can
        // change the row between the index comparison below and the update.
        sqlx::query(
            "INSERT INTO peer_delegations (master_pubkey, signing_pubkey, dm_pubkey, delegation_json, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(master_pubkey) DO UPDATE SET cached_at = cached_at",
        )
        .bind(&delegation.master_pubkey)
        .bind(&delegation.signing_pubkey)
        .bind(&delegation.dm_pubkey)
        .bind(&delegation_json)
        .bind(now)
        .execute(&mut **tx)
        .await?;

        let cached_json: String = sqlx::query_scalar(
            "SELECT delegation_json FROM peer_delegations WHERE master_pubkey = ?1",
        )
        .bind(&delegation.master_pubkey)
        .fetch_one(&mut **tx)
        .await?;
        let cached: SigningKeyDelegation = serde_json::from_str(&cached_json)?;
        if delegation.key_index < cached.key_index || delegation.dm_key_index < cached.dm_key_index
        {
            return Err(AppError::Other(format!(
                "delegation for {} is older than the cached one (key index {} < {}, dm key index {} < {})",
                proscenium_types::short_id(&delegation.master_pubkey),
                delegation.key_index,
                cached.key_index,
                delegation.dm_key_index,
                cached.dm_key_index,
            )));
        }

        sqlx::query(
            "UPDATE peer_delegations
             SET signing_pubkey = ?2, dm_pubkey = ?3, delegation_json = ?4, cached_at = ?5
             WHERE master_pubkey = ?1",
        )
        .bind(&delegation.master_pubkey)
        .bind(&delegation.signing_pubkey)
        .bind(&delegation.dm_pubkey)
        .bind(&delegation_json)
        .bind(now)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Store a device announcement that passed `verify_linked_devices_announcement`.
    ///
    /// The embedded delegation is stored under the rule of `cache_peer_delegation`
    /// (an error is returned if it is older than the cached one). The device list and the
    /// transport NodeIds are replaced only if `announcement.version` is greater than the
    /// cached version; otherwise `AnnouncementOutcome::NotNewer` is returned.
    pub async fn cache_peer_announcement(
        &self,
        announcement: &LinkedDevicesAnnouncement,
    ) -> Result<AnnouncementOutcome, AppError> {
        let mut tx = self.pool.begin().await?;
        Self::cache_peer_delegation_tx(&mut tx, &announcement.delegation).await?;

        let cached_version: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM peer_device_announcements WHERE master_pubkey = ?1",
        )
        .bind(&announcement.master_pubkey)
        .fetch_optional(&mut *tx)
        .await?;
        if cached_version.is_some_and(|v| announcement.version as i64 <= v) {
            tx.commit().await?;
            return Ok(AnnouncementOutcome::NotNewer);
        }

        let json = serde_json::to_string(announcement)?;
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO peer_device_announcements (master_pubkey, announcement_json, version, cached_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(master_pubkey) DO UPDATE SET
                announcement_json = ?2,
                version = ?3,
                cached_at = ?4",
        )
        .bind(&announcement.master_pubkey)
        .bind(&json)
        .bind(announcement.version as i64)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let transport_ids: Vec<&str> = announcement
            .devices
            .iter()
            .map(|d| d.node_id.as_str())
            .collect();
        sqlx::query(
            "UPDATE peer_delegations SET transport_node_ids_json = ?2 WHERE master_pubkey = ?1",
        )
        .bind(&announcement.master_pubkey)
        .bind(serde_json::to_string(&transport_ids)?)
        .execute(&mut *tx)
        .await?;

        // Reverse index. Rows of this master are replaced by the new device list. A NodeId
        // that is mapped to another master is reassigned to this master: the mapping of a
        // NodeId always comes from the most recently stored announcement that lists it.
        sqlx::query("DELETE FROM peer_transports WHERE master_pubkey = ?1")
            .bind(&announcement.master_pubkey)
            .execute(&mut *tx)
            .await?;
        for node_id in &transport_ids {
            sqlx::query(
                "INSERT INTO peer_transports (node_id, master_pubkey) VALUES (?1, ?2)
                 ON CONFLICT(node_id) DO UPDATE SET master_pubkey = excluded.master_pubkey",
            )
            .bind(node_id)
            .bind(&announcement.master_pubkey)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(AnnouncementOutcome::Stored)
    }

    /// Store a verified identity: delegation, device list, and profile (if present).
    /// Errors under the rule of `cache_peer_delegation`.
    pub async fn cache_peer_identity(&self, identity: &VerifiedIdentity) -> Result<(), AppError> {
        self.cache_peer_announcement(&identity.announcement).await?;
        if let Some(profile) = &identity.profile {
            self.save_profile(&identity.master_pubkey, profile).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub async fn get_peer_delegation(
        &self,
        master_pubkey: &str,
    ) -> Result<Option<SigningKeyDelegation>, AppError> {
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
    ) -> Result<Vec<String>, AppError> {
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

    /// Master pubkey whose cached delegation contains `dm_pubkey`.
    ///
    /// A delegation carries no proof that its owner holds the DM key, so two cached
    /// identities can contain the same key. In that case the owner cannot be determined and
    /// `None` is returned. `None` is also returned on a database error.
    pub async fn get_master_pubkey_for_dm_pubkey(&self, dm_pubkey: &str) -> Option<String> {
        if dm_pubkey.is_empty() {
            return None;
        }
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT master_pubkey FROM peer_delegations WHERE dm_pubkey = ?1 LIMIT 2",
        )
        .bind(dm_pubkey)
        .fetch_all(&self.pool)
        .await;
        match rows {
            Ok(mut masters) if masters.len() == 1 => masters.pop(),
            Ok(_) => None,
            Err(e) => {
                log::error!("[storage] DM pubkey lookup failed: {e}");
                None
            }
        }
    }

    /// Master pubkey that the transport NodeId `transport_id` is mapped to, from the
    /// `peer_transports` index. `Ok(None)` if no stored device list contains the NodeId.
    pub async fn try_get_master_pubkey_for_transport(
        &self,
        transport_id: &str,
    ) -> Result<Option<String>, AppError> {
        let master: Option<String> =
            sqlx::query_scalar("SELECT master_pubkey FROM peer_transports WHERE node_id = ?1")
                .bind(transport_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(master)
    }

    /// `try_get_master_pubkey_for_transport` with a database error mapped to `None`.
    /// A caller that grants access based on the result must use the `try_` variant.
    pub async fn get_master_pubkey_for_transport(&self, transport_id: &str) -> Option<String> {
        match self.try_get_master_pubkey_for_transport(transport_id).await {
            Ok(master) => master,
            Err(e) => {
                log::error!("[storage] transport NodeId lookup failed: {e}");
                None
            }
        }
    }

    pub async fn get_peer_signing_pubkey(
        &self,
        master_pubkey: &str,
    ) -> Result<Option<String>, AppError> {
        let result: Option<String> = sqlx::query_scalar(
            "SELECT signing_pubkey FROM peer_delegations WHERE master_pubkey = ?1",
        )
        .bind(master_pubkey)
        .fetch_optional(&self.pool)
        .await?;
        Ok(result)
    }

    pub async fn get_peer_dm_pubkey(
        &self,
        master_pubkey: &str,
    ) -> Result<Option<String>, AppError> {
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
