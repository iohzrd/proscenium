use crate::error::AppError;
use iroh_social_types::{
    DeviceEntry, LinkBundleData, LinkedDevicesAnnouncement, RatchetSessionExport,
    SigningKeyDelegation, now_millis,
};
use sqlx::Row;

use super::Storage;

impl Storage {
    pub async fn upsert_linked_device(
        &self,
        node_id: &str,
        device_name: &str,
        is_primary: bool,
        is_self: bool,
        added_at: u64,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO linked_devices (node_id, device_name, is_primary, is_self, added_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(node_id) DO UPDATE SET
                device_name = ?2,
                is_primary = ?3,
                is_self = ?4,
                last_seen_at = ?6",
        )
        .bind(node_id)
        .bind(device_name)
        .bind(is_primary as i32)
        .bind(is_self as i32)
        .bind(added_at as i64)
        .bind(now_millis() as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn get_linked_devices(&self) -> Result<Vec<DeviceEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT node_id, device_name, is_primary, added_at FROM linked_devices ORDER BY added_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut devices = Vec::new();
        for row in &rows {
            devices.push(DeviceEntry {
                node_id: row.get(0),
                device_name: row.get(1),
                is_primary: row.get::<i32, _>(2) != 0,
                added_at: row.get::<i64, _>(3) as u64,
            });
        }
        Ok(devices)
    }

    pub async fn cache_peer_device_announcement(
        &self,
        master_pubkey: &str,
        announcement: &LinkedDevicesAnnouncement,
    ) -> Result<(), AppError> {
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
        .bind(master_pubkey)
        .bind(&json)
        .bind(announcement.version as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;

        // Also update transport_node_ids_json in peer_delegations
        let transport_ids: Vec<&str> = announcement
            .devices
            .iter()
            .map(|d| d.node_id.as_str())
            .collect();
        let transport_json = serde_json::to_string(&transport_ids)?;
        sqlx::query(
            "UPDATE peer_delegations SET transport_node_ids_json = ?2 WHERE master_pubkey = ?1",
        )
        .bind(master_pubkey)
        .bind(&transport_json)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_peer_announcement_version(
        &self,
        master_pubkey: &str,
    ) -> Result<Option<u64>, AppError> {
        let result: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM peer_device_announcements WHERE master_pubkey = ?1",
        )
        .bind(master_pubkey)
        .fetch_optional(&self.pool)
        .await?;
        Ok(result.map(|v| v as u64))
    }

    pub async fn next_device_index(&self) -> Result<u32, AppError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM linked_devices")
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u32)
    }

    pub async fn get_all_bookmark_ids(&self) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query("SELECT post_id FROM bookmarks ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn export_ratchet_sessions(&self) -> Result<Vec<RatchetSessionExport>, AppError> {
        let rows = sqlx::query(
            "SELECT peer_pubkey, state_json, COALESCE(updated_at, 0) FROM dm_ratchet_sessions",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut sessions = Vec::new();
        for row in &rows {
            sessions.push(RatchetSessionExport {
                peer_pubkey: row.get(0),
                state_json: row.get(1),
                updated_at: row.get::<i64, _>(2) as u64,
            });
        }
        Ok(sessions)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn export_link_bundle(
        &self,
        master_pubkey: &str,
        signing_secret_key_bytes: &[u8; 32],
        dm_secret_key_bytes: &[u8; 32],
        delegation: &SigningKeyDelegation,
        transport_secret_key_bytes: &[u8; 32],
        device_index: u32,
        master_secret_key_bytes: Option<&[u8; 32]>,
    ) -> Result<LinkBundleData, AppError> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;

        let profile = self.get_profile(master_pubkey).await.ok().flatten();
        let follows = self.get_follows().await.unwrap_or_default();
        let bookmarks = self.get_all_bookmark_ids().await.unwrap_or_default();
        let blocked_users = self.get_blocked_pubkeys().await.unwrap_or_default();
        let muted_users = self.get_muted_pubkeys().await.unwrap_or_default();
        let ratchet_sessions = self.export_ratchet_sessions().await.unwrap_or_default();

        Ok(LinkBundleData {
            signing_secret_key: b64.encode(signing_secret_key_bytes),
            dm_secret_key: b64.encode(dm_secret_key_bytes),
            delegation: delegation.clone(),
            transport_secret_key: b64.encode(transport_secret_key_bytes),
            device_index,
            master_secret_key: master_secret_key_bytes.map(|k| b64.encode(k)),
            profile,
            follows,
            bookmarks,
            blocked_users,
            muted_users,
            ratchet_sessions,
        })
    }

    pub async fn import_link_bundle(
        &self,
        master_pubkey: &str,
        bundle: &LinkBundleData,
    ) -> Result<(), AppError> {
        if let Some(ref profile) = bundle.profile {
            self.save_profile(master_pubkey, profile).await?;
        }

        for follow in &bundle.follows {
            self.follow(follow).await?;
        }

        for post_id in &bundle.bookmarks {
            let _ = self.toggle_bookmark(post_id).await;
        }

        for pubkey in &bundle.blocked_users {
            self.block_user(pubkey).await?;
        }

        for pubkey in &bundle.muted_users {
            self.mute_user(pubkey).await?;
        }

        for session in &bundle.ratchet_sessions {
            self.save_ratchet_session(&session.peer_pubkey, &session.state_json, now_millis())
                .await?;
        }

        Ok(())
    }

    pub async fn get_other_device_node_ids(&self) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query(
            "SELECT node_id FROM linked_devices WHERE is_self = 0 ORDER BY added_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }
}
