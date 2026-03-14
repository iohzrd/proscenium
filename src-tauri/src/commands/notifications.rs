use crate::constants::DEFAULT_NOTIFICATION_LIMIT;
use crate::error::CmdResult;
use crate::state::AppState;
use iroh_social_types::Notification;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_notifications(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    before: Option<u64>,
) -> CmdResult<Vec<Notification>> {
    let notifications = state
        .storage
        .get_notifications(limit.unwrap_or(DEFAULT_NOTIFICATION_LIMIT), before)
        .await?;
    Ok(notifications)
}

#[tauri::command]
pub async fn get_unread_notification_count(state: State<'_, Arc<AppState>>) -> CmdResult<u32> {
    let count = state.storage.get_unread_notification_count().await?;
    Ok(count)
}

#[tauri::command]
pub async fn mark_notifications_read(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.storage.mark_notifications_read().await?;
    Ok(())
}
