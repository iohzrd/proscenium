use crate::ext::ResultExt;
use crate::state::AppState;
use iroh_social_types::{FollowEntry, FollowerEntry, now_millis, short_id};
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Resolve a peer's identity from a transport NodeId.
/// Connects to the peer, queries identity, caches the result.
async fn resolve_peer_identity(
    state: &AppState,
    node_id: &str,
) -> Result<iroh_social_types::IdentityResponse, String> {
    let target: iroh::EndpointId = node_id.parse().str_err()?;
    match crate::peer::query_identity(&state.net.endpoint, target).await {
        Ok(identity) => {
            log::info!(
                "[identity] resolved {} -> master={}",
                short_id(node_id),
                short_id(&identity.master_pubkey),
            );
            let _ = state.storage.cache_peer_identity(&identity).await;
            if let Some(profile) = &identity.profile {
                let _ = state
                    .storage
                    .save_profile(&identity.master_pubkey, profile)
                    .await;
            }
            Ok(identity)
        }
        Err(e) => Err(format!(
            "failed to query identity from {}: {e}",
            short_id(node_id),
        )),
    }
}

#[tauri::command]
pub async fn follow_user(
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> Result<(), String> {
    let my_id = state.identity.master_pubkey.clone();
    let my_transport = state.identity.transport_node_id.clone();
    if node_id == my_transport {
        return Err("cannot follow yourself".to_string());
    }
    log::info!("[follow] resolving identity for {}...", short_id(&node_id));

    // Connect to the transport NodeId and resolve identity
    let peer_identity = resolve_peer_identity(&state, &node_id).await?;
    let pubkey = peer_identity.master_pubkey.clone();

    if pubkey == my_id {
        return Err("cannot follow yourself".to_string());
    }

    let node_ids = if peer_identity.transport_node_ids.is_empty() {
        vec![node_id.clone()]
    } else {
        peer_identity.transport_node_ids.clone()
    };

    log::info!("[follow] following {}...", short_id(&pubkey));

    let entry = FollowEntry {
        pubkey: pubkey.clone(),
        alias: None,
        followed_at: now_millis(),
    };
    state.storage.follow(&entry).await.str_err()?;

    state
        .net
        .gossip
        .follow_user(pubkey.clone(), node_ids.clone())
        .await
        .str_err()?;
    log::info!("[follow] subscribed to gossip for {}", short_id(&pubkey));

    // Sync posts from the followed peer
    log::info!("[follow] syncing posts from {}...", short_id(&pubkey));
    if let Err(e) = crate::sync::sync_one_peer(
        &state.net.endpoint,
        &state.storage,
        &pubkey,
        &my_id,
        &app_handle,
        "follow-sync",
    )
    .await
    {
        log::error!(
            "[follow-sync] failed to sync from {}: {e}",
            short_id(&pubkey)
        );
    }

    Ok(())
}

#[tauri::command]
pub async fn unfollow_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    log::info!("[follow] unfollowing {}...", short_id(&pubkey));
    state.storage.unfollow(&pubkey).await.str_err()?;
    state.net.gossip.unfollow_user(&pubkey).await;
    let deleted = state
        .storage
        .delete_posts_by_author(&pubkey)
        .await
        .unwrap_or(0);
    log::info!(
        "[follow] unfollowed {}, deleted {deleted} posts",
        short_id(&pubkey)
    );
    Ok(())
}

#[tauri::command]
pub async fn update_follow_alias(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
    alias: Option<String>,
) -> Result<(), String> {
    state
        .storage
        .update_follow_alias(&pubkey, alias.as_deref())
        .await
        .str_err()
}

#[tauri::command]
pub async fn get_follows(state: State<'_, Arc<AppState>>) -> Result<Vec<FollowEntry>, String> {
    state.storage.get_follows().await.str_err()
}

#[tauri::command]
pub async fn get_followers(state: State<'_, Arc<AppState>>) -> Result<Vec<FollowerEntry>, String> {
    state.storage.get_followers().await.str_err()
}

#[tauri::command]
pub async fn get_peer_node_ids(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<Vec<String>, String> {
    state
        .storage
        .get_peer_transport_node_ids(&pubkey)
        .await
        .str_err()
}
