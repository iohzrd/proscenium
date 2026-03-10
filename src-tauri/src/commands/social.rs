use crate::ext::ResultExt;
use crate::state::AppState;
use iroh_social_types::{FollowEntry, FollowerEntry, now_millis, short_id};
use std::sync::Arc;
use tauri::{AppHandle, State};

use super::sync::process_sync_result;

/// Resolve a peer's identity from a transport NodeId.
/// Connects to the peer, queries identity, caches the result.
async fn resolve_peer_identity(
    state: &AppState,
    node_id: &str,
) -> Result<iroh_social_types::IdentityResponse, String> {
    let target: iroh::EndpointId = node_id.parse().str_err()?;
    match crate::peer::query_identity(&state.endpoint, target).await {
        Ok(identity) => {
            log::info!(
                "[identity] resolved {} -> master={}",
                short_id(node_id),
                short_id(&identity.master_pubkey),
            );
            let _ = state.storage.cache_peer_identity(&identity);
            if let Some(profile) = &identity.profile {
                let _ = state.storage.save_profile(&identity.master_pubkey, profile);
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
    let my_id = state.master_pubkey.clone();
    let my_transport = state.transport_node_id.clone();
    if node_id == my_transport {
        return Err("cannot follow yourself".to_string());
    }
    log::info!("[follow] resolving identity for {}...", short_id(&node_id));

    // Connect to the transport NodeId and resolve identity
    let identity = resolve_peer_identity(&state, &node_id).await?;
    let pubkey = identity.master_pubkey.clone();

    if pubkey == my_id {
        return Err("cannot follow yourself".to_string());
    }

    let node_ids = if identity.transport_node_ids.is_empty() {
        vec![node_id.clone()]
    } else {
        identity.transport_node_ids.clone()
    };

    log::info!("[follow] following {}...", short_id(&pubkey));

    let entry = FollowEntry {
        pubkey: pubkey.clone(),
        alias: None,
        followed_at: now_millis(),
    };
    state.storage.follow(&entry).str_err()?;

    {
        let mut feed = state.feed.lock().await;
        feed.follow_user(pubkey.clone(), &node_ids)
            .await
            .str_err()?;
    }
    log::info!("[follow] subscribed to gossip for {}", short_id(&pubkey));

    // Sync posts using the first available transport NodeId
    log::info!("[follow] syncing posts from {}...", short_id(&pubkey));
    let endpoint = state.endpoint.clone();
    let storage = state.storage.clone();
    if let Some(first_node_id) = node_ids.first()
        && let Ok(target) = first_node_id.parse::<iroh::EndpointId>()
    {
        match crate::sync::sync_from_peer(&endpoint, &storage, target, &pubkey).await {
            Ok(result) => {
                let stored = process_sync_result(
                    &storage,
                    &pubkey,
                    &result,
                    "follow-sync",
                    &my_id,
                    &app_handle,
                );
                log::info!(
                    "[follow-sync] stored {stored}/{} posts, {} interactions from {} (mode={:?})",
                    result.posts.len(),
                    result.interactions.len(),
                    short_id(&pubkey),
                    result.mode,
                );
            }
            Err(e) => {
                log::error!(
                    "[follow-sync] failed to sync from {}: {e}",
                    short_id(&pubkey)
                );
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn unfollow_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    log::info!("[follow] unfollowing {}...", short_id(&pubkey));
    state.storage.unfollow(&pubkey).str_err()?;
    let mut feed = state.feed.lock().await;
    feed.unfollow_user(&pubkey);
    let deleted = state.storage.delete_posts_by_author(&pubkey).unwrap_or(0);
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
        .str_err()
}

#[tauri::command]
pub async fn get_follows(state: State<'_, Arc<AppState>>) -> Result<Vec<FollowEntry>, String> {
    state.storage.get_follows().str_err()
}

#[tauri::command]
pub async fn get_followers(state: State<'_, Arc<AppState>>) -> Result<Vec<FollowerEntry>, String> {
    state.storage.get_followers().str_err()
}

#[tauri::command]
pub async fn get_peer_node_ids(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<Vec<String>, String> {
    state.storage.get_peer_transport_node_ids(&pubkey).str_err()
}
