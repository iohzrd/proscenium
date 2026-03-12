use crate::ext::ResultExt;
use crate::state::AppState;
use iroh_social_types::short_id;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontendSyncResult {
    pub posts: Vec<iroh_social_types::Post>,
    pub remote_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub local_count: u64,
}

#[tauri::command]
pub async fn sync_posts(
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<FrontendSyncResult, String> {
    let my_id = state.master_pubkey.clone();
    let result = crate::sync::sync_one_peer(
        &state.endpoint,
        &state.storage,
        &pubkey,
        &my_id,
        &app_handle,
        "sync",
    )
    .await
    .map_err(|e| format!("sync failed for {}: {e}", short_id(&pubkey)))?;

    Ok(FrontendSyncResult {
        posts: result.posts,
        remote_total: result.remote_post_count,
    })
}

#[tauri::command]
pub async fn get_sync_status(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<SyncStatus, String> {
    let local_count = state
        .storage
        .count_posts_by_author(&pubkey)
        .await
        .str_err()?;
    Ok(SyncStatus { local_count })
}
