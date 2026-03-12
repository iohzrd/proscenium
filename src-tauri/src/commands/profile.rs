use crate::commands::servers::sync_profile_inner;
use crate::ext::ResultExt;
use crate::state::AppState;
use iroh::SecretKey;
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
    Ok(state.transport_node_id.clone())
}

#[tauri::command]
pub async fn get_pubkey(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    Ok(state.master_pubkey.clone())
}

#[tauri::command]
pub async fn get_my_profile(state: State<'_, Arc<AppState>>) -> Result<Option<Profile>, String> {
    let node_id = state.master_pubkey.clone();
    state.storage.get_profile(&node_id).await.str_err()
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
    let node_id = state.master_pubkey.clone();
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

    // Sign with signing key
    let sk = SecretKey::from_bytes(&state.signing_secret_key_bytes);
    sign_profile(&mut profile, &sk);

    let old_visibility = state
        .storage
        .get_visibility(&node_id)
        .await
        .unwrap_or(Visibility::Public);

    let mut feed = state.feed.write().await;

    if old_visibility != new_visibility {
        // Handle gossip feed start/stop BEFORE saving new visibility
        feed.handle_visibility_change(old_visibility, new_visibility, &profile)
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

    // Broadcast profile update (gossip for Public, push outbox for Listed/Private)
    feed.broadcast_profile(&profile).await.str_err()?;

    // Sync profile to all registered discovery servers
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
    let addr = state.endpoint.addr();
    let relay_url = addr.relay_urls().next().map(|u| u.to_string());
    let has_relay = relay_url.is_some();
    let feed = state.feed.read().await;
    let follow_count = feed.subscriptions.len();
    let follower_count = state
        .storage
        .get_followers()
        .await
        .map(|f| f.len())
        .unwrap_or(0);

    Ok(NodeStatus {
        node_id: state.transport_node_id.clone(),
        has_relay,
        relay_url,
        follow_count,
        follower_count,
    })
}
