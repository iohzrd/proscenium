use crate::error::AppError;
use proscenium_types::{
    DeviceEntry, LinkBundleData, LinkedDevicesAnnouncement, MAX_DEVICE_NAME_LEN,
    MAX_REMOVED_DEVICES, MAX_TIMESTAMP_DRIFT_MS, now_millis, short_id, truncate_chars,
    validate_profile_fields, verify_linked_devices_announcement,
};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashSet;

use super::Storage;

/// Time within which a device that received a link bundle from this device is stored at
/// its first device sync.
const PENDING_LINK_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// `preferences` key of the highest version of a device list of the local identity.
pub(super) const OWN_ANNOUNCEMENT_VERSION_KEY: &str = "own_announcement_version";

/// Result of `Storage::merge_removed_devices`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RemovedMerge {
    /// Rows of other devices deleted because the list removed their NodeId.
    pub devices_removed: u32,
    /// The list removed the local device. Nothing was stored.
    pub local_removed: bool,
}

/// Removed NodeIds each device keeps. The device list carries the newest
/// `MAX_REMOVED_DEVICES` of them.
const MAX_STORED_REMOVED_DEVICES: i64 = 256;

/// Delete the records of `removed_devices` except the newest `MAX_STORED_REMOVED_DEVICES`.
async fn trim_removed_devices(tx: &mut Transaction<'_, Sqlite>) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM removed_devices WHERE node_id NOT IN (
             SELECT node_id FROM removed_devices ORDER BY removed_at DESC, node_id LIMIT ?1
         )",
    )
    .bind(MAX_STORED_REMOVED_DEVICES)
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
    /// The `added_at` of an inserted row is at least `added_at_floor`. The own device list
    /// holds the `MAX_LINKED_DEVICES` rows with the lowest `added_at`, so device sync passes
    /// the current time: a removed device that still holds the master key and syncs with
    /// a device that has not merged its removal then cannot push stored devices out of
    /// that list by naming devices with an early `added_at`.
    ///
    /// The caller passes only entries of a device list signed by the local master key.
    pub async fn insert_missing_linked_devices(
        &self,
        devices: &[DeviceEntry],
        added_at_floor: u64,
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
            .bind(i64::try_from(device.added_at.max(added_at_floor)).unwrap_or(i64::MAX))
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
    /// `removed_devices`, which keeps the newest `MAX_STORED_REMOVED_DEVICES` records. Returns
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
    /// Keeps the newest `MAX_STORED_REMOVED_DEVICES` records.
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

    /// The newest `MAX_REMOVED_DEVICES` recorded removed NodeIds, newest first: the
    /// removed NodeIds of the local device list.
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

    /// Record `version` of a device list of the local identity, built here or received
    /// from another own device. The stored value only increases, and a version more than
    /// `MAX_TIMESTAMP_DRIFT_MS` after the local clock is not recorded.
    pub async fn record_own_announcement_version(&self, version: u64) -> Result<(), AppError> {
        if version > now_millis().saturating_add(MAX_TIMESTAMP_DRIFT_MS) {
            return Ok(());
        }
        let version = i64::try_from(version).unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT INTO preferences (key, value) VALUES (?1, CAST(?2 AS TEXT))
             ON CONFLICT(key) DO UPDATE SET
                value = CAST(MAX(CAST(value AS INTEGER), ?2) AS TEXT)",
        )
        .bind(OWN_ANNOUNCEMENT_VERSION_KEY)
        .bind(version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The highest version recorded by `record_own_announcement_version`, or 0.
    pub async fn get_own_announcement_version(&self) -> Result<u64, AppError> {
        Ok(self
            .get_preference(OWN_ANNOUNCEMENT_VERSION_KEY)
            .await?
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0))
    }

    /// Record that this device sent a link bundle to the new device `node_id` at `now`.
    pub async fn record_pending_link(&self, node_id: &str, now: u64) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO pending_link_devices (node_id, linked_at) VALUES (?1, ?2)
             ON CONFLICT(node_id) DO UPDATE SET linked_at = excluded.linked_at",
        )
        .bind(node_id)
        .bind(i64::try_from(now).unwrap_or(i64::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete the record of `record_pending_link` for `node_id` and return true if it
    /// existed and is at most `PENDING_LINK_TTL_MS` old at `now`. Older records are
    /// deleted too.
    pub async fn take_pending_link(&self, node_id: &str, now: u64) -> Result<bool, AppError> {
        let cutoff = i64::try_from(now.saturating_sub(PENDING_LINK_TTL_MS)).unwrap_or(0);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM pending_link_devices WHERE linked_at < ?1")
            .bind(cutoff)
            .execute(&mut *tx)
            .await?;
        let taken = sqlx::query("DELETE FROM pending_link_devices WHERE node_id = ?1")
            .bind(node_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        tx.commit().await?;
        Ok(taken)
    }

    /// The row of `node_id` if it is stored as another device of the local user.
    pub async fn get_other_device(&self, node_id: &str) -> Result<Option<DeviceEntry>, AppError> {
        let row = sqlx::query(
            "SELECT node_id, device_name, added_at FROM linked_devices
             WHERE node_id = ?1 AND is_self = 0",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| DeviceEntry {
            node_id: row.get(0),
            device_name: row.get(1),
            added_at: row.get::<i64, _>(2) as u64,
        }))
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
    /// are recorded first. The profile is stored if its fields pass
    /// `validate_profile_fields`, also with an `updated_at` after the local clock: a
    /// missing own profile reads as Public, so the new device of a Private user would
    /// serve its posts to anyone. A peer announcement is stored only if it verifies and
    /// belongs to an actively followed user.
    pub async fn import_link_bundle(
        &self,
        master_pubkey: &str,
        own_device: &DeviceEntry,
        bundle: &LinkBundleData,
    ) -> Result<(), AppError> {
        if let Some(profile) = &bundle.profile {
            match validate_profile_fields(profile) {
                Ok(()) => {
                    self.save_profile(master_pubkey, profile).await?;
                }
                Err(reason) => log::warn!("[link] profile in bundle not stored: {reason}"),
            }
        }

        self.record_own_announcement_version(bundle.announcement.version)
            .await?;
        self.merge_removed_devices(&bundle.announcement.removed, &own_device.node_id)
            .await?;
        // The bundle comes from the device the user links with: its order is kept.
        self.insert_missing_linked_devices(&bundle.announcement.devices, 0)
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
