use crate::error::CmdResult;
use crate::state::{AppState, SyncCommand};
use proscenium_types::{FrontendSyncResult, SyncStatus};
use std::sync::Arc;
use tauri::State;

/// Sync the content of `pubkey` now. Returns the newest newly stored posts (at most
/// `SYNC_RETURNED_POSTS_MAX`, newest first) and the post count the remote reported.
#[tauri::command]
pub async fn sync_posts(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<FrontendSyncResult> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    let ep = state.endpoint();
    let result = crate::sync::sync_one_peer(
        &ep,
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

/// Ask the peer sync task to sync every followed user. Fails if its queue is full.
#[tauri::command]
pub async fn sync_all_peers(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state
        .sync_tx()
        .try_send(SyncCommand::SyncAll)
        .map_err(|_| "sync task unavailable or busy")?;
    Ok(())
}
