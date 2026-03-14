use crate::error::CmdResult;
use crate::state::AppState;
use iroh_social_types::{FollowRequestEntry, FollowResponse, now_millis, short_id};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_follow_requests(
    state: State<'_, Arc<AppState>>,
) -> CmdResult<Vec<FollowRequestEntry>> {
    let requests = state.storage.get_follow_requests().await?;
    Ok(requests)
}

#[tauri::command]
pub async fn get_pending_follow_request_count(state: State<'_, Arc<AppState>>) -> CmdResult<u64> {
    let count = state.storage.get_pending_follow_request_count().await?;
    Ok(count)
}

#[tauri::command]
pub async fn approve_follow_request(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<bool> {
    log::info!("[follow-req] approving request from {}", short_id(&pubkey));
    let approved = state.storage.approve_follow_request(&pubkey).await?;
    if approved {
        let now = now_millis();
        let _ = state.storage.upsert_follower(&pubkey, now).await;
    }
    Ok(approved)
}

#[tauri::command]
pub async fn deny_follow_request(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<bool> {
    log::info!("[follow-req] denying request from {}", short_id(&pubkey));
    let denied = state.storage.deny_follow_request(&pubkey).await?;
    Ok(denied)
}

#[tauri::command]
pub async fn send_follow_request_to_peer(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<String> {
    log::info!(
        "[follow-req] sending follow request to {}",
        short_id(&pubkey)
    );
    let node_ids = state.storage.get_peer_transport_node_ids(&pubkey).await?;
    let first_node_id = node_ids
        .first()
        .ok_or_else(|| format!("no transport NodeIds cached for {}", short_id(&pubkey)))?;
    let target: iroh::EndpointId = first_node_id.parse()?;

    let (master_pubkey, signing_secret_key_bytes, delegation) = {
        let id = state.identity.read().await;
        (
            id.master_pubkey.clone(),
            id.signing_secret_key_bytes,
            id.delegation.clone(),
        )
    };

    let response = crate::peer::send_follow_request(
        &state.endpoint,
        &state.storage,
        target,
        &master_pubkey,
        &signing_secret_key_bytes,
        &delegation,
    )
    .await?;

    let result = match response {
        FollowResponse::Approved(_) => "approved",
        FollowResponse::Denied => "denied",
        FollowResponse::Pending => "pending",
    };
    log::info!("[follow-req] response from {}: {result}", short_id(&pubkey));
    Ok(result.to_string())
}
