use crate::error::AppError;
use crate::storage::Storage;
use proscenium_types::{LinkedDevicesAnnouncement, VerifiedIdentity, now_millis};

/// Result of offering a peer's device announcement to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnouncementOutcome {
    /// The device list, the DM pubkey and the transport NodeIds were replaced.
    Stored,
    /// The cached announcement has an equal or higher version; nothing was changed.
    NotNewer,
}

impl Storage {
    /// Store a device announcement that passed `verify_linked_devices_announcement`.
    ///
    /// The announcement, its DM pubkey and the `peer_transports` rows of its master pubkey
    /// are replaced only if `announcement.version` is greater than the cached version;
    /// otherwise `AnnouncementOutcome::NotNewer` is returned.
    pub async fn cache_peer_announcement(
        &self,
        announcement: &LinkedDevicesAnnouncement,
    ) -> Result<AnnouncementOutcome, AppError> {
        let json = serde_json::to_string(announcement)?;
        let mut tx = self.pool.begin().await?;
        // The version comparison is part of the first statement, a write, so no other
        // writer can store an announcement between the comparison and the update.
        let stored = sqlx::query(
            "INSERT INTO peer_device_announcements
                (master_pubkey, announcement_json, version, dm_pubkey, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(master_pubkey) DO UPDATE SET
                announcement_json = excluded.announcement_json,
                version = excluded.version,
                dm_pubkey = excluded.dm_pubkey,
                cached_at = excluded.cached_at
             WHERE excluded.version > peer_device_announcements.version",
        )
        .bind(&announcement.master_pubkey)
        .bind(&json)
        .bind(i64::try_from(announcement.version).unwrap_or(i64::MAX))
        .bind(&announcement.dm_pubkey)
        .bind(now_millis() as i64)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if !stored {
            tx.commit().await?;
            return Ok(AnnouncementOutcome::NotNewer);
        }

        // Reverse index. Rows of this master are replaced by the new device list. A NodeId
        // that is mapped to another master is reassigned to this master: the mapping of a
        // NodeId always comes from the most recently stored announcement that lists it.
        sqlx::query("DELETE FROM peer_transports WHERE master_pubkey = ?1")
            .bind(&announcement.master_pubkey)
            .execute(&mut *tx)
            .await?;
        for device in &announcement.devices {
            sqlx::query(
                "INSERT INTO peer_transports (node_id, master_pubkey) VALUES (?1, ?2)
                 ON CONFLICT(node_id) DO UPDATE SET master_pubkey = excluded.master_pubkey",
            )
            .bind(&device.node_id)
            .bind(&announcement.master_pubkey)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(AnnouncementOutcome::Stored)
    }

    /// Store a verified identity: device list (under the rule of
    /// `cache_peer_announcement`) and profile (if present).
    pub async fn cache_peer_identity(&self, identity: &VerifiedIdentity) -> Result<(), AppError> {
        self.cache_peer_announcement(&identity.announcement).await?;
        if let Some(profile) = &identity.profile {
            self.save_profile(&identity.master_pubkey, profile).await?;
        }
        Ok(())
    }

    /// Transport NodeIds of the cached device list of `master_pubkey`, in list order.
    ///
    /// Read from the announcement itself, not from `peer_transports`: an announcement of
    /// another master that lists the same NodeIds changes the reverse index only.
    pub async fn get_peer_transport_node_ids(
        &self,
        master_pubkey: &str,
    ) -> Result<Vec<String>, AppError> {
        let node_ids = sqlx::query_scalar(
            "SELECT json_extract(d.value, '$.node_id')
             FROM peer_device_announcements a,
                  json_each(CASE WHEN json_valid(a.announcement_json)
                                 THEN a.announcement_json ELSE '{}' END, '$.devices') d
             WHERE a.master_pubkey = ?1 AND json_type(d.value, '$.node_id') = 'text'
             ORDER BY d.key",
        )
        .bind(master_pubkey)
        .fetch_all(&self.pool)
        .await?;
        Ok(node_ids)
    }

    /// Master pubkey whose cached device list contains `dm_pubkey`.
    ///
    /// A device list carries no proof that its owner holds the DM key, so two cached
    /// identities can contain the same key. In that case the owner cannot be determined and
    /// `None` is returned. `None` is also returned on a database error.
    pub async fn get_master_pubkey_for_dm_pubkey(&self, dm_pubkey: &str) -> Option<String> {
        if dm_pubkey.is_empty() {
            return None;
        }
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT master_pubkey FROM peer_device_announcements WHERE dm_pubkey = ?1 LIMIT 2",
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

    /// DM pubkey from the cached device list of `master_pubkey`.
    pub async fn get_peer_dm_pubkey(
        &self,
        master_pubkey: &str,
    ) -> Result<Option<String>, AppError> {
        let result: Option<String> = sqlx::query_scalar(
            "SELECT dm_pubkey FROM peer_device_announcements WHERE master_pubkey = ?1",
        )
        .bind(master_pubkey)
        .fetch_optional(&self.pool)
        .await?;
        Ok(result.filter(|pubkey| !pubkey.is_empty()))
    }
}
