use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use proscenium_types::{
    DeviceEntry, IdentityResponse, LinkedDevicesAnnouncement, MAX_DEVICE_NAME_LEN,
    MAX_LINKED_DEVICES, now_millis, sign_linked_devices_announcement, sign_profile, truncate_chars,
    verify_linked_devices_announcement, verify_profile_signature,
};

/// Name of the local device: the name of the operating system it runs on.
pub fn local_device_name() -> String {
    match std::env::consts::OS {
        "android" => "Android",
        "ios" => "iOS",
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
    .to_string()
}

/// Return an error unless `announcement` passes `verify_linked_devices_announcement`, is
/// signed by `master_pubkey`, and lists the device `node_id`.
pub fn verify_lists_device(
    announcement: &LinkedDevicesAnnouncement,
    master_pubkey: &str,
    node_id: &str,
) -> Result<(), AppError> {
    if announcement.master_pubkey != master_pubkey {
        return Err("device list of another identity".into());
    }
    verify_linked_devices_announcement(announcement).map_err(AppError::Other)?;
    if !announcement.devices.iter().any(|d| d.node_id == node_id) {
        return Err("device list does not list the device".into());
    }
    Ok(())
}

/// Build and sign the local user's device announcement from the `linked_devices` and
/// `removed_devices` tables.
///
/// The local device is listed first, whether or not the table holds a row for it, then
/// the other rows, at most `MAX_LINKED_DEVICES` in total. `removed` holds the recorded
/// removed NodeIds that are not listed as devices, newest first. `version` is the creation
/// time in milliseconds, so receivers replace an earlier announcement with a later one.
pub async fn build_own_announcement(
    storage: &Storage,
    identity: &SharedIdentity,
) -> Result<LinkedDevicesAnnouncement, AppError> {
    let stored = storage.get_linked_devices().await?;
    let mut removed = storage.get_removed_device_ids().await?;
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
        device_name: local_device_name(),
        added_at: own_added_at,
    });
    devices.extend(
        stored
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

    removed.retain(|node_id| !devices.iter().any(|d| d.node_id == *node_id));

    let mut announcement = LinkedDevicesAnnouncement {
        master_pubkey: id.master_pubkey.clone(),
        devices,
        removed,
        version: now,
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
    use crate::state::Identity;
    use iroh::SecretKey;
    use std::sync::Arc;

    fn node_id(seed: u8) -> String {
        SecretKey::from_bytes(&[seed; 32]).public().to_string()
    }

    fn shared_identity(master: [u8; 32], transport_seed: u8) -> SharedIdentity {
        Arc::new(tokio::sync::RwLock::new(Identity::new(
            master,
            SecretKey::from_bytes(&[transport_seed; 32]),
        )))
    }

    #[tokio::test]
    async fn announcement_lists_the_local_device_first_then_the_stored_rows() {
        let master = [5u8; 32];
        let storage = Storage::open_in_memory().await.unwrap();
        let identity = shared_identity(master, 2);
        let own = node_id(2);

        // Rows as a link bundle import leaves them: the other devices, then the local one.
        storage
            .insert_missing_linked_devices(&[
                DeviceEntry {
                    node_id: node_id(10),
                    device_name: "Linux".to_string(),
                    added_at: 10,
                },
                DeviceEntry {
                    node_id: node_id(11),
                    device_name: "Android".to_string(),
                    added_at: 20,
                },
            ])
            .await
            .unwrap();
        storage
            .upsert_linked_device(&own, "renamed", true, 30)
            .await
            .unwrap();

        let announcement = build_own_announcement(&storage, &identity).await.unwrap();
        let master_pubkey = identity.read().await.master_pubkey.clone();
        verify_lists_device(&announcement, &master_pubkey, &own).unwrap();

        let ids: Vec<String> = announcement
            .devices
            .iter()
            .map(|d| d.node_id.clone())
            .collect();
        assert_eq!(ids, vec![own, node_id(10), node_id(11)]);
        assert_eq!(announcement.devices[0].device_name, local_device_name());
        assert_eq!(announcement.devices[0].added_at, 30);

        // Without a stored row the local device is listed alone.
        let empty = Storage::open_in_memory().await.unwrap();
        let announcement = build_own_announcement(&empty, &identity).await.unwrap();
        assert_eq!(announcement.devices.len(), 1);
        assert_eq!(announcement.devices[0].node_id, node_id(2));
    }

    #[tokio::test]
    async fn verify_lists_device_requires_the_identity_and_the_device() {
        let storage = Storage::open_in_memory().await.unwrap();
        let identity = shared_identity([5u8; 32], 2);
        let master_pubkey = identity.read().await.master_pubkey.clone();
        let announcement = build_own_announcement(&storage, &identity).await.unwrap();
        verify_lists_device(&announcement, &master_pubkey, &node_id(2)).unwrap();

        // Another device, another identity, a changed list.
        assert!(verify_lists_device(&announcement, &master_pubkey, &node_id(3)).is_err());
        assert!(verify_lists_device(&announcement, &node_id(9), &node_id(2)).is_err());
        let mut forged = announcement;
        forged.devices[0].node_id = node_id(3);
        assert!(verify_lists_device(&forged, &master_pubkey, &node_id(3)).is_err());
    }
}
