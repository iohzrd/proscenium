use crate::error::AppError;
use proscenium_types::{
    DeviceEntry, LinkBundleData, LinkedDevicesAnnouncement, MAX_DEVICE_NAME_LEN,
    MAX_REMOVED_DEVICES, now_millis, short_id, truncate_chars, validate_profile,
    verify_linked_devices_announcement,
};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashSet;

use super::Storage;

/// Result of `Storage::merge_removed_devices`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RemovedMerge {
    /// Rows of other devices deleted because the list removed their NodeId.
    pub devices_removed: u32,
    /// The list removed the local device. Nothing was stored.
    pub local_removed: bool,
}

/// Delete the records of `removed_devices` except the newest `MAX_REMOVED_DEVICES`.
async fn trim_removed_devices(tx: &mut Transaction<'_, Sqlite>) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM removed_devices WHERE node_id NOT IN (
             SELECT node_id FROM removed_devices ORDER BY removed_at DESC, node_id LIMIT ?1
         )",
    )
    .bind(MAX_REMOVED_DEVICES as i64)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Return `name` cut to at most `MAX_DEVICE_NAME_LEN` bytes on a character boundary.
/// `verify_linked_devices_announcement` rejects an announcement with a longer name.
fn bounded_device_name(name: &str) -> &str {
    if name.len() > MAX_DEVICE_NAME_LEN {
        truncate_chars(name, MAX_DEVICE_NAME_LEN / 4)
    } else {
        name
    }
}

impl Storage {
    pub async fn upsert_linked_device(
        &self,
        node_id: &str,
        device_name: &str,
        is_self: bool,
        added_at: u64,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO linked_devices (node_id, device_name, is_self, added_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id) DO UPDATE SET
                device_name = ?2,
                is_self = ?3,
                last_seen_at = ?5",
        )
        .bind(node_id)
        .bind(bounded_device_name(device_name))
        .bind(is_self as i32)
        .bind(added_at as i64)
        .bind(now_millis() as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert the given devices with `is_self = 0`. A NodeId that is already stored keeps
    /// its row; a NodeId in `removed_devices` is not inserted. Returns the number of rows
    /// inserted.
    ///
    /// The caller passes only entries of a device list signed by the local master key.
    pub async fn insert_missing_linked_devices(
        &self,
        devices: &[DeviceEntry],
    ) -> Result<u32, AppError> {
        let mut inserted = 0u32;
        for device in devices {
            let result = sqlx::query(
                "INSERT INTO linked_devices (node_id, device_name, is_self, added_at, last_seen_at)
                 SELECT ?1, ?2, 0, ?3, 0
                 WHERE NOT EXISTS (SELECT 1 FROM removed_devices WHERE node_id = ?1)
                 ON CONFLICT(node_id) DO NOTHING",
            )
            .bind(&device.node_id)
            .bind(bounded_device_name(&device.device_name))
            .bind(i64::try_from(device.added_at).unwrap_or(i64::MAX))
            .execute(&self.pool)
            .await?;
            if result.rows_affected() > 0 {
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    pub async fn get_linked_devices(&self) -> Result<Vec<DeviceEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT node_id, device_name, added_at FROM linked_devices ORDER BY added_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut devices = Vec::new();
        for row in &rows {
            devices.push(DeviceEntry {
                node_id: row.get(0),
                device_name: row.get(1),
                added_at: row.get::<i64, _>(2) as u64,
            });
        }
        Ok(devices)
    }

    /// Remove the other device `node_id`: delete its row and record its NodeId in
    /// `removed_devices`, which keeps the newest `MAX_REMOVED_DEVICES` records. Returns
    /// false, and changes nothing, unless `node_id` is stored as another device.
    pub async fn remove_linked_device(
        &self,
        node_id: &str,
        removed_at: u64,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin().await?;
        let deleted = sqlx::query("DELETE FROM linked_devices WHERE node_id = ?1 AND is_self = 0")
            .bind(node_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        if !deleted {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO removed_devices (node_id, removed_at) VALUES (?1, ?2)
             ON CONFLICT(node_id) DO UPDATE SET removed_at = excluded.removed_at",
        )
        .bind(node_id)
        .bind(i64::try_from(removed_at).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await?;
        trim_removed_devices(&mut tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Store the removed NodeIds of a device list signed by the local master key: record
    /// each in `removed_devices` (a recorded NodeId keeps its time) and delete its row.
    /// Keeps the newest `MAX_REMOVED_DEVICES` records.
    ///
    /// If `removed` contains `own_node_id`, the NodeId of the local device, nothing is
    /// stored and `local_removed` is set.
    pub async fn merge_removed_devices(
        &self,
        removed: &[String],
        own_node_id: &str,
    ) -> Result<RemovedMerge, AppError> {
        if removed.iter().any(|node_id| node_id == own_node_id) {
            return Ok(RemovedMerge {
                devices_removed: 0,
                local_removed: true,
            });
        }
        if removed.is_empty() {
            return Ok(RemovedMerge::default());
        }
        let now = i64::try_from(now_millis()).unwrap_or(i64::MAX);
        let mut tx = self.pool.begin().await?;
        let mut devices_removed = 0u32;
        for node_id in removed {
            sqlx::query(
                "INSERT INTO removed_devices (node_id, removed_at) VALUES (?1, ?2)
                 ON CONFLICT(node_id) DO NOTHING",
            )
            .bind(node_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            let deleted =
                sqlx::query("DELETE FROM linked_devices WHERE node_id = ?1 AND is_self = 0")
                    .bind(node_id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
            devices_removed += u32::try_from(deleted).unwrap_or(u32::MAX);
        }
        trim_removed_devices(&mut tx).await?;
        tx.commit().await?;
        Ok(RemovedMerge {
            devices_removed,
            local_removed: false,
        })
    }

    /// The recorded removed NodeIds, newest first.
    pub async fn get_removed_device_ids(&self) -> Result<Vec<String>, AppError> {
        let ids = sqlx::query_scalar(
            "SELECT node_id FROM removed_devices ORDER BY removed_at DESC, node_id LIMIT ?1",
        )
        .bind(MAX_REMOVED_DEVICES as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }

    /// True if `node_id` is recorded in `removed_devices`.
    pub async fn is_removed_device(&self, node_id: &str) -> Result<bool, AppError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM removed_devices WHERE node_id = ?1)")
                .bind(node_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    /// True if `node_id` is stored as another device of the local user.
    pub async fn is_other_device(&self, node_id: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM linked_devices WHERE node_id = ?1 AND is_self = 0)",
        )
        .bind(node_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn get_other_device_node_ids(&self) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query(
            "SELECT node_id FROM linked_devices WHERE is_self = 0 ORDER BY added_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// True if the database holds rows written for an existing identity:
    /// the row of the local device, or any profile.
    pub async fn has_identity_data(&self) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM linked_devices WHERE is_self = 1)
                 OR EXISTS(SELECT 1 FROM profiles)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Cached signed device announcements of the users that `me` follows.
    /// A stored announcement that does not parse is left out.
    pub async fn get_followed_peer_announcements(
        &self,
        me: &str,
    ) -> Result<Vec<LinkedDevicesAnnouncement>, AppError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT a.announcement_json FROM peer_device_announcements a
             JOIN social_graph s ON s.followee = a.master_pubkey
             WHERE s.follower = ?1 AND s.state = 'active' AND s.followed_at IS NOT NULL",
        )
        .bind(me)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|json| serde_json::from_str(json).ok())
            .collect())
    }

    /// Store the content of a link bundle for the identity `master_pubkey`.
    ///
    /// `own_device` is the row of the local device. The caller has checked that
    /// `bundle.announcement` is signed by the bundle's master key. Its removed NodeIds
    /// are recorded first. The profile is stored only if it passes `validate_profile`; a
    /// peer announcement is stored only if it verifies and belongs to an actively followed
    /// user.
    pub async fn import_link_bundle(
        &self,
        master_pubkey: &str,
        own_device: &DeviceEntry,
        bundle: &LinkBundleData,
    ) -> Result<(), AppError> {
        if let Some(profile) = &bundle.profile {
            match validate_profile(profile) {
                Ok(()) => {
                    self.save_profile(master_pubkey, profile).await?;
                }
                Err(reason) => log::warn!("[link] profile in bundle not stored: {reason}"),
            }
        }

        self.merge_removed_devices(&bundle.announcement.removed, &own_device.node_id)
            .await?;
        self.insert_missing_linked_devices(&bundle.announcement.devices)
            .await?;
        self.upsert_linked_device(
            &own_device.node_id,
            &own_device.device_name,
            true,
            own_device.added_at,
        )
        .await?;

        self.merge_follows_lww(master_pubkey, &bundle.follows)
            .await?;
        self.merge_moderation_lww(&bundle.moderation).await?;
        self.merge_bookmarks_lww(&bundle.bookmarks).await?;
        self.merge_follow_requests(master_pubkey, &bundle.follow_requests)
            .await?;

        let followed: HashSet<&str> = bundle
            .follows
            .iter()
            .filter(|f| f.state == "active")
            .map(|f| f.pubkey.as_str())
            .collect();
        for announcement in &bundle.peer_announcements {
            if !followed.contains(announcement.master_pubkey.as_str()) {
                continue;
            }
            if let Err(reason) = verify_linked_devices_announcement(announcement) {
                log::warn!(
                    "[link] announcement of {} in bundle not stored: {reason}",
                    short_id(&announcement.master_pubkey)
                );
                continue;
            }
            if let Err(e) = self.cache_peer_announcement(announcement, None).await {
                log::warn!(
                    "[link] announcement of {} in bundle not stored: {e}",
                    short_id(&announcement.master_pubkey)
                );
            }
        }

        Ok(())
    }
}
