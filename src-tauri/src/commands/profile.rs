use crate::error::CmdResult;
use crate::state::AppState;
use iroh::SecretKey;
use proscenium_types::{NodeStatus, Profile, Visibility, sign_profile, validate_profile};
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
    let (node_id, signing_key_bytes) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.signing_secret_key_bytes)
    };
    let new_visibility: Visibility = visibility.parse()?;
    let mut profile = Profile {
        display_name: display_name.clone(),
        bio: bio.clone(),
        avatar_hash,
        avatar_ticket,
        visibility: new_visibility,
        signature: String::new(),
    };
    validate_profile(&profile)?;
    sign_profile(&mut profile, &SecretKey::from_bytes(&signing_key_bytes));

    let old_visibility = state
        .storage
        .get_visibility(&node_id)
        .await
        .unwrap_or(Visibility::Public);

    if old_visibility != new_visibility {
        state
            .gossip
            .handle_visibility_change(old_visibility, new_visibility, &profile)
            .await?;
        log::info!("[profile] visibility transition: {old_visibility} -> {new_visibility}");
    }

    state.storage.save_profile(&node_id, &profile).await?;
    log::info!("[profile] saved profile: {display_name} (visibility={new_visibility})");

    state.gossip.broadcast_profile(&profile).await?;

    if let Ok(servers) = state.storage.get_servers().await {
        for server in servers {
            if server.registered_at.is_some() {
                let _ = crate::commands::servers::sync_profile_to_server_inner(
                    &state.http_client,
                    &state.storage,
                    &state.identity,
                    &server.url,
                )
                .await;
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
    state.storage.get_profile(&pubkey).await
}

#[tauri::command]
pub async fn get_node_status(state: State<'_, Arc<AppState>>) -> CmdResult<NodeStatus> {
    let transport_node_id = state.identity.read().await.transport_node_id.clone();
    let addr = state.endpoint.addr();
    let relay_url = addr.relay_urls().next().map(|u| u.to_string());
    let has_relay = relay_url.is_some();
    let follow_count = state.gossip.get_subscription_count().await;
    let follower_count = state
        .storage
        .get_followers()
        .await
        .map(|f| f.len())
        .unwrap_or(0);

    Ok(NodeStatus {
        node_id: transport_node_id,
        has_relay,
        relay_url,
        follow_count,
        follower_count,
    })
}
