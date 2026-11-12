use crate::error::AppError;
use crate::storage::Storage;
use proscenium_types::{
    LinkedDevicesAnnouncement, VerifiedIdentity, announcement_cache_version, now_millis,
};

/// Result of offering a peer's device announcement to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnouncementOutcome {
    /// The device list and the transport NodeIds were replaced.
    Stored,
    /// The cached announcement has an equal or higher version; nothing was changed.
    NotNewer,
}

impl Storage {
    /// Store a device announcement that passed `verify_linked_devices_announcement`.
    ///
    /// The announcement replaces the cached one of its master pubkey only if its
    /// `announcement_cache_version` (the version, at most `MAX_TIMESTAMP_DRIFT_MS` after
    /// the local clock) is greater than the stored one; otherwise
    /// `AnnouncementOutcome::NotNewer` is returned.
    ///
    /// `peer_transports` maps a NodeId to a master pubkey only if the NodeId delivered,
    /// over a connection that QUIC authenticates, a device list of that master that the
    /// cached list also contains. A signed list proves that its master named the NodeIds,
    /// not that the devices agreed, so a list from any other source changes no mapping.
    /// `delivered_by` is the NodeId of that connection, or `None` for a list received
    /// through gossip or a link bundle. When a newer list is stored, the mappings of
    /// NodeIds it no longer contains are removed.
    pub async fn cache_peer_announcement(
        &self,
        announcement: &LinkedDevicesAnnouncement,
        delivered_by: Option<&str>,
    ) -> Result<AnnouncementOutcome, AppError> {
        let json = serde_json::to_string(announcement)?;
        let master = &announcement.master_pubkey;
        let mut tx = self.pool.begin().await?;
        // The version comparison is part of the first statement, a write, so no other
        // writer can store an announcement between the comparison and the update.
        let stored = sqlx::query(
            "INSERT INTO peer_device_announcements
                (master_pubkey, announcement_json, version, cached_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(master_pubkey) DO UPDATE SET
                announcement_json = excluded.announcement_json,
                version = excluded.version,
                cached_at = excluded.cached_at
             WHERE excluded.version > peer_device_announcements.version",
        )
        .bind(master)
        .bind(&json)
        .bind(i64::try_from(announcement_cache_version(announcement)).unwrap_or(i64::MAX))
        .bind(now_millis() as i64)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;

        if stored {
            sqlx::query(
                "DELETE FROM peer_transports WHERE master_pubkey = ?1
                 AND node_id NOT IN (
                     SELECT json_extract(d.value, '$.node_id') FROM json_each(?2, '$.devices') d
                 )",
            )
            .bind(master)
            .bind(&json)
            .execute(&mut *tx)
            .await?;
        }

        // The cached list decides, so an older list delivered by a device maps it only
        // if the newer cached list still contains it.
        if let Some(node_id) = delivered_by
            && announcement.devices.iter().any(|d| d.node_id == node_id)
        {
            sqlx::query(
                "INSERT INTO peer_transports (node_id, master_pubkey)
                 SELECT ?1, ?2 WHERE EXISTS (
                     SELECT 1 FROM peer_device_announcements a,
                          json_each(a.announcement_json, '$.devices') d
                     WHERE a.master_pubkey = ?2 AND json_extract(d.value, '$.node_id') = ?1
                 )
                 ON CONFLICT(node_id) DO UPDATE SET master_pubkey = excluded.master_pubkey",
            )
            .bind(node_id)
            .bind(master)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(if stored {
            AnnouncementOutcome::Stored
        } else {
            AnnouncementOutcome::NotNewer
        })
    }

    /// Store a verified identity: device list (under the rule of
    /// `cache_peer_announcement`, delivered by `identity.node_id`) and profile (if
    /// present).
    pub async fn cache_peer_identity(&self, identity: &VerifiedIdentity) -> Result<(), AppError> {
        self.cache_peer_announcement(&identity.announcement, Some(&identity.node_id))
            .await?;
        if let Some(profile) = &identity.profile {
            self.save_profile(&identity.master_pubkey, profile).await?;
        }
        Ok(())
    }

    /// Transport NodeIds of the cached device list of `master_pubkey`, in list order.
    ///
    /// Read from the announcement itself, not from `peer_transports`, which holds only
    /// the NodeIds that delivered a list themselves.
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

    /// Master pubkey that the transport NodeId `transport_id` is mapped to, from the
    /// `peer_transports` index (see `cache_peer_announcement`). `Ok(None)` if the NodeId
    /// is not mapped.
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

    /// Delete the cached device lists, and their NodeId mappings, that were stored before
    /// `cached_before` and whose user is not followed by or following `me`, has no DM
    /// conversation and no follow request. Returns the number of lists deleted.
    pub async fn prune_peer_announcements(
        &self,
        me: &str,
        cached_before: u64,
    ) -> Result<u64, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let stale = "SELECT master_pubkey FROM peer_device_announcements a
             WHERE a.cached_at < ?2
               AND NOT EXISTS (SELECT 1 FROM social_graph s
                   WHERE s.state = 'active'
                     AND ((s.follower = ?1 AND s.followee = a.master_pubkey)
                       OR (s.followee = ?1 AND s.follower = a.master_pubkey)))
               AND NOT EXISTS (SELECT 1 FROM dm_conversations c
                   WHERE c.peer_pubkey = a.master_pubkey)
               AND NOT EXISTS (SELECT 1 FROM follow_requests r
                   WHERE r.pubkey = a.master_pubkey)";
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM peer_transports WHERE master_pubkey IN ({stale})"
        )))
        .bind(me)
        .bind(i64::try_from(cached_before).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await?;
        let deleted = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM peer_device_announcements WHERE master_pubkey IN ({stale})"
        )))
        .bind(me)
        .bind(i64::try_from(cached_before).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(deleted)
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
}
