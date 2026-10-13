use super::{as_storage_cursor, before_cursor};
use crate::constants::DEFAULT_NOTIFICATION_LIMIT;
use crate::error::CmdResult;
use crate::state::AppState;
use proscenium_types::Notification;
use std::sync::Arc;
use tauri::State;

/// Notifications, newest first. `before` and `before_id` are the timestamp and id of the
/// last notification of the previous page.
#[tauri::command]
pub async fn get_notifications(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    before: Option<u64>,
    before_id: Option<String>,
) -> CmdResult<Vec<Notification>> {
    let cursor = before_cursor(before, before_id);
    let notifications = state
        .storage
        .get_notifications(
            limit.unwrap_or(DEFAULT_NOTIFICATION_LIMIT),
            as_storage_cursor(&cursor),
        )
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
