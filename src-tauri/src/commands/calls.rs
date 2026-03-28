use crate::audio::android::{self, CommAudioDevice};
use crate::error::CmdResult;
use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn start_call(state: State<'_, Arc<AppState>>, peer_pubkey: String) -> CmdResult<String> {
    let call_id = state.call().start_call(&peer_pubkey).await?;
    Ok(call_id)
}

#[tauri::command]
pub async fn accept_call(state: State<'_, Arc<AppState>>, call_id: String) -> CmdResult<()> {
    state.call().accept_call(&call_id).await
}

#[tauri::command]
pub async fn reject_call(state: State<'_, Arc<AppState>>, call_id: String) -> CmdResult<()> {
    state.call().reject_call(&call_id).await
}

#[tauri::command]
pub async fn hangup_call(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.call().hangup().await
}

#[tauri::command]
pub async fn toggle_mute_call(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    state.call().toggle_mute().await
}

#[tauri::command]
pub async fn switch_call_input_device(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> CmdResult<()> {
    state.call().switch_input_device(&name).await
}

#[tauri::command]
pub async fn switch_call_output_device(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> CmdResult<()> {
    state.call().switch_output_device(&name).await
}

/// List available Android communication audio devices.
#[tauri::command]
pub fn list_android_audio_devices() -> CmdResult<Vec<CommAudioDevice>> {
    android::list_communication_devices().map_err(crate::error::AppError::Other)
}

/// Switch Android audio route by device ID.
/// Sets the OS-level route, then rebuilds the cpal streams so they
/// pick up the new routing.
#[tauri::command]
pub async fn set_android_audio_device(
    state: State<'_, Arc<AppState>>,
    device_id: i32,
) -> CmdResult<()> {
    android::set_communication_device_by_id(device_id).map_err(crate::error::AppError::Other)?;

    // Android disconnects existing AAudio streams when the communication
    // device changes (onAudioDeviceUpdate -> DISCONNECT). We must wait for
    // that to happen, then rebuild the cpal streams on the new device.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    state.call().rebuild_streams().await;
    Ok(())
}
