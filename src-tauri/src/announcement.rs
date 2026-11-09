use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use proscenium_types::{
    DeviceEntry, IdentityResponse, LinkedDevicesAnnouncement, MAX_DEVICE_NAME_LEN,
    MAX_LINKED_DEVICES, now_millis, sign_linked_devices_announcement, sign_profile, truncate_chars,
    verify_profile_signature,
};

/// Name stored for a device that has the transport key derivation index `device_index`.
pub fn default_device_name(device_index: u32) -> String {
    if device_index == 0 {
        "Primary Device".to_string()
    } else {
        format!("Device {device_index}")
    }
}

/// Build and sign the local user's device announcement from the `linked_devices` table.
///
/// The local device is listed first, whether or not the table holds a row for it. Every
/// other row is listed if its NodeId derives from the local master key; `is_primary` is
/// set from the derivation index. `version` is the creation time in milliseconds, so
/// receivers replace an earlier announcement with a later one.
pub async fn build_own_announcement(
    storage: &Storage,
    identity: &SharedIdentity,
) -> Result<LinkedDevicesAnnouncement, AppError> {
    let stored = storage.get_linked_devices().await?;
    let id = identity.read().await;
    let now = now_millis();

    let mut devices: Vec<DeviceEntry> = Vec::with_capacity(stored.len() + 1);
    let own_added_at = stored
        .iter()
        .find(|d| d.node_id == id.transport_node_id)
        .map(|d| d.added_at)
        .unwrap_or(now);
    devices.push(DeviceEntry {
        node_id: id.transport_node_id.clone(),
        device_name: default_device_name(id.device_index),
        is_primary: id.device_index == 0,
        added_at: own_added_at,
    });
    devices.extend(
        id.own_devices(&stored)
            .into_iter()
            .filter(|d| d.node_id != id.transport_node_id),
    );
    devices.truncate(MAX_LINKED_DEVICES);
    for device in &mut devices {
        if device.device_name.len() > MAX_DEVICE_NAME_LEN {
            device.device_name =
                truncate_chars(&device.device_name, MAX_DEVICE_NAME_LEN / 4).to_string();
        }
    }

    let mut announcement = LinkedDevicesAnnouncement {
        master_pubkey: id.master_pubkey.clone(),
        dm_pubkey: id.dm_pubkey.clone(),
        devices,
        version: now,
        timestamp: now,
        signature: String::new(),
    };
    sign_linked_devices_announcement(&mut announcement, &id.master_key);
    Ok(announcement)
}

/// Build the local user's `IdentityResponse`: the signed device list and the stored profile.
///
/// If the stored profile's signature does not verify under the master key, the profile
/// is signed again and saved.
pub async fn build_own_identity_response(
    storage: &Storage,
    identity: &SharedIdentity,
) -> Result<IdentityResponse, AppError> {
    let announcement = build_own_announcement(storage, identity).await?;
    let (master_pubkey, master_key) = {
        let id = identity.read().await;
        (id.master_pubkey.clone(), id.master_key.clone())
    };

    let mut profile = storage.get_profile(&master_pubkey).await?;
    if let Some(p) = profile.as_mut()
        && verify_profile_signature(p, &master_pubkey).is_err()
    {
        sign_profile(p, &master_pubkey, &master_key);
        storage.save_profile(&master_pubkey, p).await?;
    }

    Ok(IdentityResponse {
        announcement,
        profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Identity, device_node_id};
    use proscenium_types::verify_linked_devices_announcement;
    use std::sync::Arc;

    fn shared_identity(master: [u8; 32], device_index: u32) -> SharedIdentity {
        Arc::new(tokio::sync::RwLock::new(Identity::derive(
            master,
            device_index,
        )))
    }

    #[tokio::test]
    async fn linked_device_announces_every_own_device_and_itself() {
        let master = [5u8; 32];
        let storage = Storage::open_in_memory().await.unwrap();
        let identity = shared_identity(master, 2);
        let own = device_node_id(&master, 2);

        // Rows as a link bundle import leaves them: the other devices, then the local one.
        storage
            .insert_missing_linked_devices(&[
                DeviceEntry {
                    node_id: device_node_id(&master, 0),
                    device_name: "Primary Device".to_string(),
                    is_primary: true,
                    added_at: 10,
                },
                DeviceEntry {
                    node_id: device_node_id(&master, 1),
                    device_name: "Device 1".to_string(),
                    is_primary: false,
                    added_at: 20,
                },
            ])
            .await
            .unwrap();
        storage
            .upsert_linked_device(&own, "Device 2", false, true, 30)
            .await
            .unwrap();
        // A row whose NodeId does not derive from the master key is not announced.
        let foreign = device_node_id(&[6u8; 32], 0);
        storage
            .upsert_linked_device(&foreign, "Other", false, false, 40)
            .await
            .unwrap();

        let announcement = build_own_announcement(&storage, &identity).await.unwrap();
        verify_linked_devices_announcement(&announcement).unwrap();

        let ids: Vec<&str> = announcement
            .devices
            .iter()
            .map(|d| d.node_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                own.as_str(),
                device_node_id(&master, 0).as_str(),
                device_node_id(&master, 1).as_str(),
            ]
        );
        let primaries: Vec<bool> = announcement.devices.iter().map(|d| d.is_primary).collect();
        assert_eq!(primaries, vec![false, true, false]);
        assert_eq!(announcement.devices[0].device_name, "Device 2");
    }

    #[tokio::test]
    async fn announcement_lists_local_device_without_a_stored_row() {
        let master = [8u8; 32];
        let storage = Storage::open_in_memory().await.unwrap();

        let linked = shared_identity(master, 3);
        let announcement = build_own_announcement(&storage, &linked).await.unwrap();
        verify_linked_devices_announcement(&announcement).unwrap();
        assert_eq!(announcement.devices.len(), 1);
        assert_eq!(announcement.devices[0].node_id, device_node_id(&master, 3));
        assert!(!announcement.devices[0].is_primary);
        assert_eq!(announcement.devices[0].device_name, "Device 3");

        let primary = shared_identity(master, 0);
        let announcement = build_own_announcement(&storage, &primary).await.unwrap();
        assert!(announcement.devices[0].is_primary);
        assert_eq!(announcement.devices[0].device_name, "Primary Device");
    }
}
