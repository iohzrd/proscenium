use crate::ext::ResultExt;
use crate::state::AppState;
use crate::storage::follow_requests::FollowRequestEntry;
use iroh_social_types::{FollowResponse, now_millis, short_id};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_follow_requests(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<FollowRequestEntry>, String> {
    state.storage.get_follow_requests().str_err()
}

#[tauri::command]
pub async fn get_pending_follow_request_count(
    state: State<'_, Arc<AppState>>,
) -> Result<u64, String> {
    state.storage.get_pending_follow_request_count().str_err()
}

#[tauri::command]
pub async fn approve_follow_request(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<bool, String> {
    log::info!("[follow-req] approving request from {}", short_id(&pubkey));
    let approved = state.storage.approve_follow_request(&pubkey).str_err()?;
    if approved {
        // Also add them as a follower
        let now = now_millis();
        let _ = state.storage.upsert_follower(&pubkey, now);
    }
    Ok(approved)
}

#[tauri::command]
pub async fn deny_follow_request(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<bool, String> {
    log::info!("[follow-req] denying request from {}", short_id(&pubkey));
    state.storage.deny_follow_request(&pubkey).str_err()
}

#[tauri::command]
pub async fn send_follow_request_to_peer(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<String, String> {
    log::info!(
        "[follow-req] sending follow request to {}",
        short_id(&pubkey)
    );
    let target: iroh::EndpointId = pubkey.parse().str_err()?;
    let response =
        crate::peer::send_follow_request(&state.endpoint, target, &state.secret_key_bytes)
            .await
            .str_err()?;

    let result = match response {
        FollowResponse::Approved => "approved",
        FollowResponse::Denied => "denied",
        FollowResponse::Pending => "pending",
    };
    log::info!("[follow-req] response from {}: {result}", short_id(&pubkey));
    Ok(result.to_string())
}
