use crate::error::CmdResult;
use crate::state::AppState;
use proscenium_types::{
    NodeStatus, Profile, Visibility, now_millis, sign_profile, validate_profile,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_node_id(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    Ok(state.identity.read().await.transport_node_id.clone())
}

#[tauri::command]
pub async fn get_pubkey(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    Ok(state.identity.read().await.master_pubkey.clone())
}

#[tauri::command]
pub async fn get_my_profile(state: State<'_, Arc<AppState>>) -> CmdResult<Option<Profile>> {
    let master_pubkey = state.identity.read().await.master_pubkey.clone();
    state.storage.get_profile(&master_pubkey).await
}

#[tauri::command]
pub async fn save_my_profile(
    state: State<'_, Arc<AppState>>,
    display_name: String,
    bio: String,
    avatar_hash: Option<String>,
    avatar_ticket: Option<String>,
    visibility: String,
) -> CmdResult<()> {
    let (node_id, master_key) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.master_key.clone())
    };
    let new_visibility: Visibility = visibility.parse()?;
    // `save_profile` only replaces a row with an equal or greater `updated_at`, so the new
    // value must exceed the stored one even if the system clock moved backwards.
    let stored_updated_at = state
        .storage
        .get_profile(&node_id)
        .await?
        .map_or(0, |p| p.updated_at);
    let mut profile = Profile {
        display_name: display_name.clone(),
        bio: bio.clone(),
        avatar_hash,
        avatar_ticket,
        visibility: new_visibility,
        updated_at: now_millis().max(stored_updated_at.saturating_add(1)),
        signature: String::new(),
    };
    validate_profile(&profile)?;
    sign_profile(&mut profile, &node_id, &master_key);

    let old_visibility = state.storage.get_visibility(&node_id).await?;
    if old_visibility == Visibility::Public && new_visibility != Visibility::Public {
        state.gossip().leave_public(&profile).await?;
    }

    if !state.storage.save_profile(&node_id, &profile).await? {
        return Err("profile was not saved: the stored profile is newer".into());
    }
    log::info!("[profile] saved profile: {display_name} (visibility={new_visibility})");

    state.gossip().apply_stored_visibility().await?;
    state.gossip().broadcast_profile(&profile).await?;

    if let Ok(servers) = state.storage.get_servers().await {
        for server in servers {
            if server.registered_at.is_some()
                && let Err(e) = crate::commands::servers::sync_profile_to_server_inner(
                    &state.http_client,
                    &state.storage,
                    &state.identity,
                    &server.url,
                )
                .await
            {
                log::warn!("[profile] profile sync to {} failed: {e}", server.url);
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn get_remote_profile(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<Option<Profile>> {
    super::require_pubkey(&pubkey)?;
    state.storage.get_profile(&pubkey).await
}

#[tauri::command]
pub async fn get_node_status(state: State<'_, Arc<AppState>>) -> CmdResult<NodeStatus> {
    let (transport_node_id, my_id) = {
        let id = state.identity.read().await;
        (id.transport_node_id.clone(), id.master_pubkey.clone())
    };
    let addr = state.endpoint().addr();
    let relay_url = addr.relay_urls().next().map(|u| u.to_string());
    let has_relay = relay_url.is_some();
    let follow_count = state.gossip().get_subscription_count().await;
    let follower_count = state.storage.count_followers(&my_id).await.unwrap_or(0) as usize;

    Ok(NodeStatus {
        node_id: transport_node_id,
        has_relay,
        relay_url,
        follow_count,
        follower_count,
    })
}
