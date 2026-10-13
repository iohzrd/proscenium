use crate::error::CmdResult;
use crate::peer::PeerHandler;
use crate::state::AppState;
use proscenium_types::{DeviceEntry, LinkQrPayload, short_id};
use std::sync::Arc;
use tauri::State;

/// Start a device-link session on this device. The returned payload is shown as a QR
/// code; it is accepted until `expires_at`. Linking always transfers the master key.
#[tauri::command]
pub async fn start_device_link(state: State<'_, Arc<AppState>>) -> CmdResult<LinkQrPayload> {
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
/// declined. Nothing is changed locally until the other device acknowledged the key
/// confirmation. Then all local rows are deleted, the key files are replaced, the link
/// bundle is imported and the network stack is rebuilt. The caller reloads the webview
/// after `Ok`.
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
             The keys, posts, messages, follows and settings stored on this device are \
             deleted. This cannot be undone.",
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
    let device_index = keys.device_index;

    let state_arc: Arc<AppState> = Arc::clone(&state);
    crate::setup::replace_identity(&state_arc, keys, Some(&bundle)).await?;

    log::info!("[link] device linked (device_index={device_index})");
    Ok(())
}

#[tauri::command]
pub async fn get_linked_devices(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<DeviceEntry>> {
    state.storage.get_linked_devices().await
}

#[tauri::command]
pub async fn force_device_sync(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    crate::device_sync::sync_all_devices(&state).await;
    Ok(())
}
