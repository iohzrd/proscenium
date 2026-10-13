use crate::constants::MAX_NODE_IDS_TRIED;
use crate::error::{AppError, CmdResult};
use crate::state::AppState;
use proscenium_types::{FollowRequestEntry, FollowResponse, is_valid_pubkey, now_millis, short_id};
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

/// Send a follow request to the user `pubkey` and return the answer:
/// `"approved"`, `"denied"` or `"pending"`.
///
/// The request is sent to at most `MAX_NODE_IDS_TRIED` cached NodeIds of the user, in
/// order, until one answers. Fails if no NodeId is cached or none answers.
#[tauri::command]
pub async fn send_follow_request_to_peer(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<String> {
    if !is_valid_pubkey(&pubkey) {
        return Err("invalid pubkey".into());
    }
    if pubkey == state.identity.read().await.master_pubkey {
        return Err("cannot send a follow request to yourself".into());
    }
    log::info!(
        "[follow-req] sending follow request to {}",
        short_id(&pubkey)
    );
    let node_ids = state.storage.get_peer_transport_node_ids(&pubkey).await?;
    if node_ids.is_empty() {
        return Err(format!("no transport NodeIds cached for {}", short_id(&pubkey)).into());
    }

    let ep = state.endpoint();
    let mut last_err = AppError::Other("no usable transport NodeId".into());
    for node_id in node_ids.iter().take(MAX_NODE_IDS_TRIED) {
        let Ok(target) = node_id.parse::<iroh::EndpointId>() else {
            continue;
        };
        match crate::peer::send_follow_request(
            &ep,
            &state.storage,
            &state.identity,
            target,
            &pubkey,
        )
        .await
        {
            Ok(response) => {
                let result = match response {
                    FollowResponse::Approved(_) => "approved",
                    FollowResponse::Denied => "denied",
                    FollowResponse::Pending => "pending",
                };
                log::info!("[follow-req] response from {}: {result}", short_id(&pubkey));
                return Ok(result.to_string());
            }
            Err(e) => {
                log::debug!("[follow-req] failed via {}: {e}", short_id(node_id));
                last_err = e;
            }
        }
    }
    Err(last_err)
}
