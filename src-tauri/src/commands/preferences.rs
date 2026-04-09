use crate::audio::{self, AudioDevice};
use crate::error::CmdResult;
use crate::preferences;
use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn wipe_all_data(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.storage.wipe_all_data().await
}

#[tauri::command]
pub async fn get_mdns_discovery(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    state
        .storage
        .get_bool_preference(preferences::MDNS_DISCOVERY)
        .await
}

#[tauri::command]
pub async fn set_mdns_discovery(state: State<'_, Arc<AppState>>, enabled: bool) -> CmdResult<()> {
    state
        .storage
        .set_bool_preference(preferences::MDNS_DISCOVERY, enabled)
        .await
}

#[tauri::command]
pub async fn get_dht_discovery(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    state
        .storage
        .get_bool_preference(preferences::DHT_DISCOVERY)
        .await
}

#[tauri::command]
pub async fn set_dht_discovery(state: State<'_, Arc<AppState>>, enabled: bool) -> CmdResult<()> {
    state
        .storage
        .set_bool_preference(preferences::DHT_DISCOVERY, enabled)
        .await
}

#[tauri::command]
pub async fn get_share_follows(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    Ok(preferences::get_share_follows(&state.storage).await)
}

#[tauri::command]
pub async fn set_share_follows(state: State<'_, Arc<AppState>>, enabled: bool) -> CmdResult<()> {
    state
        .storage
        .set_bool_preference(preferences::SHARE_FOLLOWS, enabled)
        .await
}

#[tauri::command]
pub async fn get_share_followers(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    Ok(preferences::get_share_followers(&state.storage).await)
}

#[tauri::command]
pub async fn set_share_followers(state: State<'_, Arc<AppState>>, enabled: bool) -> CmdResult<()> {
    state
        .storage
        .set_bool_preference(preferences::SHARE_FOLLOWERS, enabled)
        .await
}

#[tauri::command]
pub fn list_audio_input_devices() -> Vec<AudioDevice> {
    audio::list_input_devices()
}

#[tauri::command]
pub fn list_audio_output_devices() -> Vec<AudioDevice> {
    audio::list_output_devices()
}

#[tauri::command]
pub async fn get_audio_input_device(state: State<'_, Arc<AppState>>) -> CmdResult<Option<String>> {
    state
        .storage
        .get_preference(preferences::AUDIO_INPUT_DEVICE)
        .await
}

#[tauri::command]
pub async fn set_audio_input_device(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> CmdResult<()> {
    if name.is_empty() {
        // Empty string means "use default" -- remove the preference
        state
            .storage
            .set_preference(preferences::AUDIO_INPUT_DEVICE, "")
            .await
    } else {
        state
            .storage
            .set_preference(preferences::AUDIO_INPUT_DEVICE, &name)
            .await
    }
}

#[tauri::command]
pub async fn get_accent_color(state: State<'_, Arc<AppState>>) -> CmdResult<Option<String>> {
    state
        .storage
        .get_preference(preferences::ACCENT_COLOR)
        .await
}

#[tauri::command]
pub async fn set_accent_color(state: State<'_, Arc<AppState>>, name: String) -> CmdResult<()> {
    state
        .storage
        .set_preference(preferences::ACCENT_COLOR, &name)
        .await
}

#[tauri::command]
pub async fn get_audio_output_device(state: State<'_, Arc<AppState>>) -> CmdResult<Option<String>> {
    state
        .storage
        .get_preference(preferences::AUDIO_OUTPUT_DEVICE)
        .await
}

#[tauri::command]
pub async fn set_audio_output_device(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> CmdResult<()> {
    if name.is_empty() {
        state
            .storage
            .set_preference(preferences::AUDIO_OUTPUT_DEVICE, "")
            .await
    } else {
        state
            .storage
            .set_preference(preferences::AUDIO_OUTPUT_DEVICE, &name)
            .await
    }
}
