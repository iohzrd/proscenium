use crate::constants::DEFAULT_NOTIFICATION_LIMIT;
use crate::ext::ResultExt;
use crate::state::AppState;
use crate::storage::Notification;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_notifications(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    before: Option<u64>,
) -> Result<Vec<Notification>, String> {
    state
        .storage
        .get_notifications(limit.unwrap_or(DEFAULT_NOTIFICATION_LIMIT), before)
        .await
        .str_err()
}

#[tauri::command]
pub async fn get_unread_notification_count(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    state
        .storage
        .get_unread_notification_count()
        .await
        .str_err()
}

#[tauri::command]
pub async fn mark_notifications_read(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.storage.mark_notifications_read().await.str_err()
}
