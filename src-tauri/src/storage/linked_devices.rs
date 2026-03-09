use iroh_social_types::{DeviceEntry, LinkedDevicesAnnouncement, now_millis};
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
}
