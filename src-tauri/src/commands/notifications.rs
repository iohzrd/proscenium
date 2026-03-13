use crate::constants::DEFAULT_NOTIFICATION_LIMIT;
use crate::state::AppState;
use iroh_social_types::Notification;
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_notifications(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    before: Option<u64>,
) -> Result<Vec<Notification>, String> {
    state.get_notifications(limit, before).await
}

#[tauri::command]
pub async fn get_unread_notification_count(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    state.get_unread_notification_count().await
}

#[tauri::command]
pub async fn mark_notifications_read(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.mark_notifications_read().await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn get_notifications(
        &self,
        limit: Option<usize>,
        before: Option<u64>,
    ) -> Result<Vec<Notification>, String> {
        self.storage
            .get_notifications(limit.unwrap_or(DEFAULT_NOTIFICATION_LIMIT), before)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_unread_notification_count(&self) -> Result<u32, String> {
        self.storage
            .get_unread_notification_count()
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn mark_notifications_read(&self) -> Result<(), String> {
        self.storage
            .mark_notifications_read()
            .await
            .map_err(|e| e.to_string())
    }
}
