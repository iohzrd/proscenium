use crate::ext::ResultExt;
use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn toggle_bookmark(
    state: State<'_, Arc<AppState>>,
    post_id: String,
) -> Result<bool, String> {
    state.storage.toggle_bookmark(&post_id).await.str_err()
}

#[tauri::command]
pub async fn is_bookmarked(
    state: State<'_, Arc<AppState>>,
    post_id: String,
) -> Result<bool, String> {
    state.storage.is_bookmarked(&post_id).await.str_err()
}

#[tauri::command]
pub async fn mute_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    state.storage.mute_user(&pubkey).await.str_err()
}

#[tauri::command]
pub async fn unmute_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    state.storage.unmute_user(&pubkey).await.str_err()
}

#[tauri::command]
pub async fn is_muted(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<bool, String> {
    state.storage.is_muted(&pubkey).await.str_err()
}

#[tauri::command]
pub async fn get_muted_pubkeys(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    state.storage.get_muted_pubkeys().await.str_err()
}

#[tauri::command]
pub async fn block_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    let is_following = state.storage.is_following(&pubkey).await.str_err()?;

    if is_following {
        state.storage.unfollow(&pubkey).await.str_err()?;
        let mut feed = state.feed.write().await;
        feed.unfollow_user(&pubkey);
    }

    state.storage.block_user(&pubkey).await.str_err()
}

#[tauri::command]
pub async fn unblock_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    state.storage.unblock_user(&pubkey).await.str_err()
}

#[tauri::command]
pub async fn is_blocked(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<bool, String> {
    state.storage.is_blocked(&pubkey).await.str_err()
}

#[tauri::command]
pub async fn get_blocked_pubkeys(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    state.storage.get_blocked_pubkeys().await.str_err()
}
