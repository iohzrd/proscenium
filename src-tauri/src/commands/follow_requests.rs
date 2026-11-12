use crate::error::CmdResult;
use crate::state::AppState;
use proscenium_types::{FollowRequestEntry, now_millis, short_id};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_follow_requests(
    state: State<'_, Arc<AppState>>,
) -> CmdResult<Vec<FollowRequestEntry>> {
    state.storage.get_follow_requests(now_millis()).await
}

#[tauri::command]
pub async fn get_pending_follow_request_count(state: State<'_, Arc<AppState>>) -> CmdResult<u64> {
    state
        .storage
        .get_pending_follow_request_count(now_millis())
        .await
}

/// Approve the pending follow request of `pubkey` and record `pubkey` as a follower.
/// Returns `false` if no pending, unexpired request exists.
#[tauri::command]
pub async fn approve_follow_request(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<bool> {
    log::info!("[follow-req] approving request from {}", short_id(&pubkey));
    let my_id = state.identity.read().await.master_pubkey.clone();
    state
        .storage
        .approve_follow_request(&my_id, &pubkey, now_millis())
        .await
}

#[tauri::command]
pub async fn deny_follow_request(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<bool> {
    log::info!("[follow-req] denying request from {}", short_id(&pubkey));
    state
        .storage
        .deny_follow_request(&pubkey, now_millis())
        .await
}
