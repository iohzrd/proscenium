use crate::error::CmdResult;
use crate::preferences;
use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

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
