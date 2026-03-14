use crate::error::CmdResult;
use crate::state::{AppState, SyncCommand};
use iroh_social_types::{FrontendSyncResult, SyncStatus};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn sync_posts(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<FrontendSyncResult> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    let result = crate::sync::sync_one_peer(
        &state.endpoint,
        &state.storage,
        &pubkey,
        &my_id,
        &state.app_handle,
        "sync",
    )
    .await?;

    Ok(FrontendSyncResult {
        posts: result.posts,
        remote_total: result.remote_post_count,
    })
}

#[tauri::command]
pub async fn get_sync_status(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<SyncStatus> {
    let local_count = state.storage.count_posts_by_author(&pubkey).await?;
    Ok(SyncStatus { local_count })
}

#[tauri::command]
pub async fn sync_all_peers(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state
        .sync_tx
        .send(SyncCommand::SyncAll)
        .await
        .map_err(|_| "sync task unavailable")?;
    Ok(())
}
