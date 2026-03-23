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
