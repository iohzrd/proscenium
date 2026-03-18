use crate::error::CmdResult;
use crate::state::{AppState, SyncCommand};
use proscenium_types::{FollowEntry, FollowerEntry, now_millis, short_id};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn follow_user(state: State<'_, Arc<AppState>>, node_id: String) -> CmdResult<()> {
    let (my_id, my_transport) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.transport_node_id.clone())
    };
    if node_id == my_transport {
        return Err("cannot follow yourself".into());
    }
    log::info!("[follow] resolving identity for {}...", short_id(&node_id));

    let peer_identity = resolve_peer_identity(&state, &node_id).await?;
    let pubkey = peer_identity.master_pubkey.clone();

    if pubkey == my_id {
        return Err("cannot follow yourself".into());
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
    state.storage.follow(&entry).await?;
    state
        .gossip
        .follow_user(pubkey.clone(), node_ids.clone())
        .await?;
    log::info!("[follow] subscribed to gossip for {}", short_id(&pubkey));

    // Queue an immediate background sync for the new followee without blocking the command.
    let _ = state.sync_tx.send(SyncCommand::SyncPeer(pubkey)).await;

    Ok(())
}

#[tauri::command]
pub async fn unfollow_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    log::info!("[follow] unfollowing {}...", short_id(&pubkey));
    state.storage.unfollow(&pubkey).await?;
    state.gossip.unfollow_user(&pubkey).await;
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
) -> CmdResult<()> {
    state
        .storage
        .update_follow_alias(&pubkey, alias.as_deref())
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn get_follows(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<FollowEntry>> {
    state.storage.get_follows().await
}

#[tauri::command]
pub async fn get_followers(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<FollowerEntry>> {
    state.storage.get_followers().await
}

#[tauri::command]
pub async fn get_peer_node_ids(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<Vec<String>> {
    state.storage.get_peer_transport_node_ids(&pubkey).await
}

async fn resolve_peer_identity(
    state: &AppState,
    node_id: &str,
) -> CmdResult<proscenium_types::IdentityResponse> {
    let target: iroh::EndpointId = node_id.parse()?;
    match crate::peer::query_identity(&state.endpoint, target).await {
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
        Err(e) => Err(format!("failed to query identity from {}: {e}", short_id(node_id),).into()),
    }
}
