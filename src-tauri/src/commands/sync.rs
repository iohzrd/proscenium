use crate::error::CmdResult;
use crate::state::{AppState, SyncCommand};
use proscenium_types::{FrontendSyncResult, SyncStatus, now_millis, short_id};
use std::sync::Arc;
use tauri::State;

/// Sync the content of `pubkey` now. Returns the newest newly stored posts (at most
/// `SYNC_RETURNED_POSTS_MAX`, newest first) and the post count the remote reported.
///
/// For a user the local user does not follow, only the newest `MAX_VIEWED_AUTHOR_ITEMS`
/// posts and interactions are kept, and the content of at most `MAX_VIEWED_AUTHORS` such
/// users: the content of the user synced longest ago is deleted.
#[tauri::command]
pub async fn sync_posts(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<FrontendSyncResult> {
    let ep = state.endpoint();
    let result = crate::sync::sync_one_peer(
        &ep,
        &state.storage,
        &state.identity,
        &pubkey,
        &state.app_handle,
        "sync",
    )
    .await?;

    let me = state.identity.read().await.master_pubkey.clone();
    if !state.storage.is_following(&me, &pubkey).await? {
        state.storage.trim_author_content(&pubkey).await?;
        for author in state
            .storage
            .record_viewed_author(&pubkey, now_millis())
            .await?
        {
            let deleted = state
                .storage
                .delete_content_unless_followed(&me, &author)
                .await?;
            log::info!(
                "[sync] deleted {deleted} posts of {}, viewed longest ago",
                short_id(&author)
            );
        }
    }

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
