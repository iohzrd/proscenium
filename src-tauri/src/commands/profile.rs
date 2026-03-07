use crate::ext::ResultExt;
use crate::state::{AppState, NodeStatus};
use iroh_social_types::{Profile, validate_profile};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_node_id(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    Ok(state.endpoint.id().to_string())
}

#[tauri::command]
pub async fn get_my_profile(state: State<'_, Arc<AppState>>) -> Result<Option<Profile>, String> {
    let node_id = state.endpoint.id().to_string();
    state.storage.get_profile(&node_id).str_err()
}

#[tauri::command]
pub async fn save_my_profile(
    state: State<'_, Arc<AppState>>,
    display_name: String,
    bio: String,
    avatar_hash: Option<String>,
    avatar_ticket: Option<String>,
    is_private: bool,
) -> Result<(), String> {
    let node_id = state.endpoint.id().to_string();
    let profile = Profile {
        display_name: display_name.clone(),
        bio: bio.clone(),
        avatar_hash,
        avatar_ticket,
        is_private,
    };
    validate_profile(&profile)?;
    state.storage.save_profile(&node_id, &profile).str_err()?;
    log::info!("[profile] saved profile: {display_name} (private={is_private})");
    let feed = state.feed.lock().await;
    feed.broadcast_profile(&profile).await.str_err()?;
    log::info!("[profile] broadcast profile update");
    Ok(())
}

#[tauri::command]
pub async fn get_remote_profile(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<Option<Profile>, String> {
    state.storage.get_profile(&pubkey).str_err()
}

#[tauri::command]
pub async fn get_node_status(state: State<'_, Arc<AppState>>) -> Result<NodeStatus, String> {
    let addr = state.endpoint.addr();
    let relay_url = addr.relay_urls().next().map(|u| u.to_string());
    let has_relay = relay_url.is_some();
    let feed = state.feed.lock().await;
    let follow_count = feed.subscriptions.len();
    let follower_count = state.storage.get_followers().map(|f| f.len()).unwrap_or(0);

    Ok(NodeStatus {
        node_id: state.endpoint.id().to_string(),
        has_relay,
        relay_url,
        follow_count,
        follower_count,
    })
}
