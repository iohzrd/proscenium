use iroh_social_types::{
    DeviceEntry, LinkBundleData, LinkedDevicesAnnouncement, RatchetSessionExport,
    SigningKeyDelegation, now_millis,
};
use rusqlite::params;

use super::Storage;

impl Storage {
    /// Insert or update a linked device (own device registry).
    pub fn upsert_linked_device(
        &self,
        node_id: &str,
        device_name: &str,
        is_primary: bool,
        is_self: bool,
        added_at: u64,
    ) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "INSERT INTO linked_devices (node_id, device_name, is_primary, is_self, added_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(node_id) DO UPDATE SET
                    device_name = ?2,
                    is_primary = ?3,
                    is_self = ?4,
                    last_seen_at = ?6",
                params![
                    node_id,
                    device_name,
                    is_primary as i32,
                    is_self as i32,
                    added_at as i64,
                    now_millis() as i64,
                ],
            )?;
            Ok(())
        })
    }

    /// Get all linked devices.
    #[allow(dead_code)]
    pub fn get_linked_devices(&self) -> anyhow::Result<Vec<DeviceEntry>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT node_id, device_name, is_primary, added_at FROM linked_devices ORDER BY added_at ASC",
            )?;
            let mut rows = stmt.query([])?;
            let mut devices = Vec::new();
            while let Some(row) = rows.next()? {
                devices.push(DeviceEntry {
                    node_id: row.get(0)?,
                    device_name: row.get(1)?,
                    is_primary: row.get::<_, i32>(2)? != 0,
                    added_at: row.get::<_, i64>(3)? as u64,
                });
            }
            Ok(devices)
        })
    }

    /// Cache a peer's device announcement.
    pub fn cache_peer_device_announcement(
        &self,
        master_pubkey: &str,
        announcement: &LinkedDevicesAnnouncement,
    ) -> anyhow::Result<()> {
        let json = serde_json::to_string(announcement)?;
        let now = now_millis() as i64;
        self.with_db(|db| {
            db.execute(
                "INSERT INTO peer_device_announcements (master_pubkey, announcement_json, version, cached_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(master_pubkey) DO UPDATE SET
                    announcement_json = ?2,
                    version = ?3,
                    cached_at = ?4",
                params![master_pubkey, json, announcement.version as i64, now],
            )?;

            // Also update transport_node_ids_json in peer_delegations
            let transport_ids: Vec<&str> = announcement
                .devices
                .iter()
                .map(|d| d.node_id.as_str())
                .collect();
            let transport_json = serde_json::to_string(&transport_ids)?;
            db.execute(
                "UPDATE peer_delegations SET transport_node_ids_json = ?2 WHERE master_pubkey = ?1",
                params![master_pubkey, transport_json],
            )?;

            Ok(())
        })
    }

    /// Get the cached announcement version for a peer (to skip stale announcements).
    pub fn get_peer_announcement_version(
        &self,
        master_pubkey: &str,
    ) -> anyhow::Result<Option<u64>> {
        self.with_db(|db| {
            let result: Result<i64, _> = db.query_row(
                "SELECT version FROM peer_device_announcements WHERE master_pubkey = ?1",
                params![master_pubkey],
                |row| row.get(0),
            );
            match result {
                Ok(v) => Ok(Some(v as u64)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    /// Get the next available device index for a new linked device.
    pub fn next_device_index(&self) -> anyhow::Result<u32> {
        self.with_db(|db| {
            let count: i64 =
                db.query_row("SELECT COUNT(*) FROM linked_devices", [], |row| row.get(0))?;
            Ok(count as u32)
        })
    }

    /// Get all bookmark post IDs.
    pub fn get_all_bookmark_ids(&self) -> anyhow::Result<Vec<String>> {
        self.with_db(|db| {
            let mut stmt = db.prepare("SELECT post_id FROM bookmarks ORDER BY created_at DESC")?;
            let mut rows = stmt.query([])?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next()? {
                ids.push(row.get(0)?);
            }
            Ok(ids)
        })
    }

    /// Export all ratchet sessions for device pairing transfer.
    pub fn export_ratchet_sessions(&self) -> anyhow::Result<Vec<RatchetSessionExport>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT peer_pubkey, state_json, COALESCE(updated_at, 0) FROM dm_ratchet_sessions",
            )?;
            let mut rows = stmt.query([])?;
            let mut sessions = Vec::new();
            while let Some(row) = rows.next()? {
                sessions.push(RatchetSessionExport {
                    peer_pubkey: row.get(0)?,
                    state_json: row.get(1)?,
                    updated_at: row.get::<_, i64>(2)? as u64,
                });
            }
            Ok(sessions)
        })
    }

    /// Export a full link bundle for device pairing.
    pub fn export_link_bundle(
        &self,
        master_pubkey: &str,
        signing_secret_key_bytes: &[u8; 32],
        delegation: &SigningKeyDelegation,
        transport_secret_key_bytes: &[u8; 32],
        device_index: u32,
        master_secret_key_bytes: Option<&[u8; 32]>,
    ) -> anyhow::Result<LinkBundleData> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;

        let profile = self.get_profile(master_pubkey).ok().flatten();
        let follows = self.get_follows().unwrap_or_default();
        let bookmarks = self.get_all_bookmark_ids().unwrap_or_default();
        let blocked_users = self.get_blocked_pubkeys().unwrap_or_default();
        let muted_users = self.get_muted_pubkeys().unwrap_or_default();
        let ratchet_sessions = self.export_ratchet_sessions().unwrap_or_default();

        Ok(LinkBundleData {
            signing_secret_key: b64.encode(signing_secret_key_bytes),
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

    /// Import a link bundle received during device pairing.
    pub fn import_link_bundle(
        &self,
        master_pubkey: &str,
        bundle: &LinkBundleData,
    ) -> anyhow::Result<()> {
        // Import profile
        if let Some(ref profile) = bundle.profile {
            self.save_profile(master_pubkey, profile)?;
        }

        // Import follows
        for follow in &bundle.follows {
            self.follow(follow)?;
        }

        // Import bookmarks
        for post_id in &bundle.bookmarks {
            let _ = self.toggle_bookmark(post_id);
        }

        // Import blocks
        for pubkey in &bundle.blocked_users {
            self.block_user(pubkey)?;
        }

        // Import mutes
        for pubkey in &bundle.muted_users {
            self.mute_user(pubkey)?;
        }

        // Import ratchet sessions
        for session in &bundle.ratchet_sessions {
            self.save_ratchet_session(&session.peer_pubkey, &session.state_json, now_millis())?;
        }

        Ok(())
    }
}
