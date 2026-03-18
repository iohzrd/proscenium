use crate::error::CmdResult;
use crate::state::AppState;
use proscenium_types::StageState;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn create_stage(state: State<'_, Arc<AppState>>, title: String) -> CmdResult<String> {
    let ticket = state.stage.actor_handle().create_stage(title).await?;
    Ok(ticket.to_string())
}

#[tauri::command]
pub async fn join_stage(state: State<'_, Arc<AppState>>, ticket: String) -> CmdResult<()> {
    let ticket = ticket
        .parse()
        .map_err(|e: String| crate::error::AppError::Other(e))?;
    state.stage.actor_handle().join_stage(ticket).await
}

#[tauri::command]
pub async fn leave_stage(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.stage.actor_handle().leave_stage().await
}

#[tauri::command]
pub async fn end_stage(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.stage.actor_handle().end_stage().await
}

#[tauri::command]
pub async fn get_stage_state(state: State<'_, Arc<AppState>>) -> CmdResult<Option<StageState>> {
    Ok(state.stage.actor_handle().get_state().await)
}

#[tauri::command]
pub async fn stage_promote_speaker(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<()> {
    state.stage.actor_handle().promote_speaker(pubkey).await
}

#[tauri::command]
pub async fn stage_demote_speaker(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<()> {
    state.stage.actor_handle().demote_speaker(pubkey).await
}

#[tauri::command]
pub async fn stage_toggle_mute(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    state.stage.actor_handle().toggle_self_mute().await
}

#[tauri::command]
pub async fn stage_raise_hand(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.stage.actor_handle().raise_hand().await
}

#[tauri::command]
pub async fn stage_lower_hand(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.stage.actor_handle().lower_hand().await
}

#[tauri::command]
pub async fn stage_send_reaction(state: State<'_, Arc<AppState>>, emoji: String) -> CmdResult<()> {
    state.stage.actor_handle().send_reaction(emoji).await
}

#[tauri::command]
pub async fn stage_send_chat(state: State<'_, Arc<AppState>>, text: String) -> CmdResult<()> {
    state.stage.actor_handle().send_chat(text).await
}

#[tauri::command]
pub async fn stage_volunteer_relay(
    state: State<'_, Arc<AppState>>,
    capacity: u32,
) -> CmdResult<()> {
    state
        .stage
        .actor_handle()
        .volunteer_as_relay(capacity)
        .await
}
