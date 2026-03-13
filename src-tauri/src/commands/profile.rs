use crate::commands::servers::sync_profile_inner;
use crate::ext::ResultExt;
use crate::state::AppState;
use iroh_social_types::{Profile, Visibility, sign_profile, validate_profile};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    pub node_id: String,
    pub has_relay: bool,
    pub relay_url: Option<String>,
    pub follow_count: usize,
    pub follower_count: usize,
}

#[tauri::command]
pub async fn get_node_id(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    Ok(state.identity.transport_node_id.clone())
}

#[tauri::command]
pub async fn get_pubkey(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    Ok(state.identity.master_pubkey.clone())
}

#[tauri::command]
pub async fn get_my_profile(state: State<'_, Arc<AppState>>) -> Result<Option<Profile>, String> {
    state
        .storage
        .get_profile(&state.identity.master_pubkey)
        .await
        .str_err()
}

#[tauri::command]
pub async fn save_my_profile(
    state: State<'_, Arc<AppState>>,
    display_name: String,
    bio: String,
    avatar_hash: Option<String>,
    avatar_ticket: Option<String>,
    visibility: String,
) -> Result<(), String> {
    let node_id = state.identity.master_pubkey.clone();
    let new_visibility: Visibility = visibility.parse().map_err(|e: String| e)?;
    let mut profile = Profile {
        display_name: display_name.clone(),
        bio: bio.clone(),
        avatar_hash,
        avatar_ticket,
        visibility: new_visibility,
        signature: String::new(),
    };
    validate_profile(&profile)?;

    sign_profile(&mut profile, &state.identity.signing_key);

    let old_visibility = state
        .storage
        .get_visibility(&node_id)
        .await
        .unwrap_or(Visibility::Public);

    if old_visibility != new_visibility {
        state
            .net
            .gossip
            .handle_visibility_change(old_visibility, new_visibility, &profile)
            .await
            .str_err()?;
        log::info!("[profile] visibility transition: {old_visibility} -> {new_visibility}");
    }

    state
        .storage
        .save_profile(&node_id, &profile)
        .await
        .str_err()?;
    log::info!("[profile] saved profile: {display_name} (visibility={new_visibility})");

    state
        .net
        .gossip
        .broadcast_profile(&profile)
        .await
        .str_err()?;

    if let Ok(servers) = state.storage.get_servers().await {
        for server in servers {
            if server.registered_at.is_some() {
                let _ = sync_profile_inner(&state, &server.url).await;
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn get_remote_profile(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<Option<Profile>, String> {
    state.storage.get_profile(&pubkey).await.str_err()
}

#[tauri::command]
pub async fn get_node_status(state: State<'_, Arc<AppState>>) -> Result<NodeStatus, String> {
    let addr = state.net.endpoint.addr();
    let relay_url = addr.relay_urls().next().map(|u| u.to_string());
    let has_relay = relay_url.is_some();
    let follow_count = state.net.gossip.get_subscription_count().await;
    let follower_count = state
        .storage
        .get_followers()
        .await
        .map(|f| f.len())
        .unwrap_or(0);

    Ok(NodeStatus {
        node_id: state.identity.transport_node_id.clone(),
        has_relay,
        relay_url,
        follow_count,
        follower_count,
    })
}
