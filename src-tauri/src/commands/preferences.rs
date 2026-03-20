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
