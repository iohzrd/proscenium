use crate::error::CmdResult;
use crate::peer::PeerHandler;
use crate::state::AppState;
use proscenium_types::{DeviceEntry, LinkQrPayload, now_millis, parse_pubkey, short_id};
use std::sync::Arc;
use tauri::{Manager, State};

/// Start a device-link session on this device. The returned payload is shown as a QR
/// code; it is accepted until `expires_at`. Linking always transfers the master key,
/// after a second native dialog in which the user compares the link code.
///
/// Asks for confirmation in a native dialog first, which script in the webview cannot
/// answer, and returns the error `"cancelled"` if it is declined.
#[tauri::command]
pub async fn start_device_link(state: State<'_, Arc<AppState>>) -> CmdResult<LinkQrPayload> {
    let confirmed = crate::confirm::confirm_native(
        &state.app_handle,
        "Link a device",
        "Show a link code? A device that scans it within 60 seconds can request the master \
         key of this identity, which lets it post, message and link further devices as you. \
         The key is sent only after you confirm that both devices show the same code.",
        "Show link code",
    )
    .await;
    if !confirmed {
        return Err("cancelled".into());
    }
    let relay_url = state
        .endpoint()
        .addr()
        .relay_urls()
        .next()
        .map(|u| u.to_string());
    state.peer().start_link_session(relay_url).await
}

#[tauri::command]
pub async fn cancel_device_link(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.peer().cancel_link_session().await;
    Ok(())
}

/// Replace the identity of this device with the identity of the device that issued
/// `qr_payload`.
///
/// Asks for confirmation in a native dialog and returns the error `"cancelled"` if it is
/// declined. Nothing is changed locally until the link bundle was received and
/// validated. Then all local rows and media are deleted, the key files are replaced with
/// the master key and a new transport key, the link bundle is imported and the network
/// stack is rebuilt. The caller reloads the
/// webview after `Ok`.
#[tauri::command]
pub async fn link_with_device(
    state: State<'_, Arc<AppState>>,
    qr_payload: LinkQrPayload,
) -> CmdResult<()> {
    PeerHandler::check_link_payload(&qr_payload)?;

    let confirmed = crate::confirm::confirm_native(
        &state.app_handle,
        "Link this device",
        &format!(
            "Replace the identity on this device with the identity of device {}?\n\n\
             The keys, posts, messages, follows, media and settings stored on this device \
             are deleted. This cannot be undone.",
            short_id(&qr_payload.node_id)
        ),
        "Replace identity",
    )
    .await;
    if !confirmed {
        return Err("cancelled".into());
    }

    let (keys, bundle) = state
        .peer()
        .request_link(&state.endpoint(), &qr_payload)
        .await?;

    let state_arc: Arc<AppState> = Arc::clone(&state);
    crate::setup::replace_identity(&state_arc, keys, Some(&bundle)).await?;

    log::info!("[link] device linked");
    Ok(())
}

#[tauri::command]
pub async fn get_linked_devices(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<DeviceEntry>> {
    state.storage.get_linked_devices().await
}

/// Remove the other device `node_id` from the identity: record its NodeId as removed,
/// delete its row, send the new device list to followers, and start a device sync with
/// the remaining devices, which copies the removal to them. The removed device deletes
/// its data when a device sync shows it a list that removes it.
///
/// Asks for confirmation in a native dialog and returns the error `"cancelled"` if it is
/// declined.
#[tauri::command]
pub async fn remove_linked_device(
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> CmdResult<()> {
    let node_id = parse_pubkey(&node_id)?.to_string();
    if node_id == state.identity.read().await.transport_node_id {
        return Err("This device cannot remove itself. Delete all data on it instead.".into());
    }
    let Some(device) = state
        .storage
        .get_linked_devices()
        .await?
        .into_iter()
        .find(|d| d.node_id == node_id)
    else {
        return Err("Not a linked device.".into());
    };

    let confirmed = crate::confirm::confirm_native(
        &state.app_handle,
        "Remove device",
        &format!(
            "Remove {} ({}) from this identity?\n\n\
             The device deletes its data, including its copy of the master key, the next \
             time it connects to one of your devices. Until then it can still post and \
             message as you. A device that someone else controls can ignore the removal; \
             only a new identity protects against it.",
            crate::confirm::dialog_device_name(&device.device_name),
            short_id(&node_id)
        ),
        "Remove device",
    )
    .await;
    if !confirmed {
        return Err("cancelled".into());
    }

    if !state
        .storage
        .remove_linked_device(&node_id, now_millis())
        .await?
    {
        return Err("Not a linked device.".into());
    }
    log::info!("[devices] removed device {}", short_id(&node_id));

    let announcement =
        crate::announcement::build_own_announcement(&state.storage, &state.identity).await?;
    if let Err(e) = state.gossip().broadcast_linked_devices(&announcement).await {
        log::error!("[devices] failed to broadcast device announcement: {e}");
    }
    let state = Arc::clone(&state);
    tokio::spawn(async move { crate::device_sync::sync_all_devices(&state).await });
    Ok(())
}

/// True once after this device's data was deleted because another device removed it:
/// deletes the marker file `setup::DEVICE_REMOVED_FILE`.
#[tauri::command]
pub async fn take_device_removed_notice(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    let marker = state
        .app_handle
        .path()
        .app_data_dir()?
        .join(crate::setup::DEVICE_REMOVED_FILE);
    match tokio::fs::remove_file(&marker).await {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[tauri::command]
pub async fn force_device_sync(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    crate::device_sync::sync_all_devices(&state).await;
    Ok(())
}
