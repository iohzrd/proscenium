use crate::error::CmdResult;
use crate::state::{AppState, SyncCommand};
use proscenium_types::{SocialGraphEntry, now_millis, short_id};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

/// 60-minute cache staleness threshold.
const CACHE_MAX_AGE_MS: u64 = 60 * 60 * 1000;

#[derive(Serialize)]
pub struct RemoteFollowsResult {
    pub follows: Vec<SocialGraphEntry>,
    pub hidden: bool,
    pub cached: bool,
}

#[derive(Serialize)]
pub struct RemoteFollowersResult {
    pub followers: Vec<SocialGraphEntry>,
    pub hidden: bool,
    pub cached: bool,
}

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

    let entry = SocialGraphEntry {
        pubkey: pubkey.clone(),
        followed_at: now_millis(),
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    state.storage.follow(&my_id, &entry).await?;
    state
        .gossip()
        .follow_user(pubkey.clone(), node_ids.clone())
        .await?;
    log::info!("[follow] subscribed to gossip for {}", short_id(&pubkey));

    // Queue an immediate background sync for the new followee without blocking the command.
    let _ = state.sync_tx().send(SyncCommand::SyncPeer(pubkey)).await;

    Ok(())
}

#[tauri::command]
pub async fn unfollow_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    log::info!("[follow] unfollowing {}...", short_id(&pubkey));
    state.storage.unfollow(&my_id, &pubkey).await?;
    state.gossip().unfollow_user(&pubkey).await;
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
pub async fn get_follows(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<SocialGraphEntry>> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    state.storage.get_follows(&my_id).await
}

#[tauri::command]
pub async fn get_followers(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<SocialGraphEntry>> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    state.storage.get_followers(&my_id).await
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
    let ep = state.endpoint();
    match crate::peer::query_identity(&ep, target).await {
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

fn cache_is_fresh(fetched_at: Option<u64>) -> bool {
    fetched_at.is_some_and(|t| now_millis().saturating_sub(t) < CACHE_MAX_AGE_MS)
}

#[tauri::command]
pub async fn get_remote_follows(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<RemoteFollowsResult> {
    // Check cache first.
    let cached = state
        .storage
        .get_cached_remote_follows(&pubkey)
        .await
        .unwrap_or(None);

    if let Some(ref c) = cached
        && cache_is_fresh(c.fetched_at)
    {
        return Ok(RemoteFollowsResult {
            follows: c.entries.clone(),
            hidden: c.hidden,
            cached: true,
        });
    }

    // Try live fetch.
    let node_ids = state
        .storage
        .get_peer_transport_node_ids(&pubkey)
        .await
        .unwrap_or_default();

    for node_id in &node_ids {
        let Ok(target) = node_id.parse() else {
            continue;
        };
        match crate::peer::fetch_remote_follows(&state.endpoint(), target).await {
            Ok(resp) => {
                let _ = state
                    .storage
                    .cache_remote_follows(&pubkey, &resp.follows, resp.hidden)
                    .await;
                return Ok(RemoteFollowsResult {
                    follows: resp.follows,
                    hidden: resp.hidden,
                    cached: false,
                });
            }
            Err(e) => {
                log::debug!("[remote-follows] failed via {}: {e}", short_id(node_id));
            }
        }
    }

    // Fall back to stale cache.
    if let Some(c) = cached {
        return Ok(RemoteFollowsResult {
            follows: c.entries,
            hidden: c.hidden,
            cached: true,
        });
    }

    Err("peer offline and no cached data".into())
}

#[tauri::command]
pub async fn get_remote_followers(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<RemoteFollowersResult> {
    let cached = state
        .storage
        .get_cached_remote_followers(&pubkey)
        .await
        .unwrap_or(None);

    if let Some(ref c) = cached
        && cache_is_fresh(c.fetched_at)
    {
        return Ok(RemoteFollowersResult {
            followers: c.entries.clone(),
            hidden: c.hidden,
            cached: true,
        });
    }

    let node_ids = state
        .storage
        .get_peer_transport_node_ids(&pubkey)
        .await
        .unwrap_or_default();

    for node_id in &node_ids {
        let Ok(target) = node_id.parse() else {
            continue;
        };
        match crate::peer::fetch_remote_followers(&state.endpoint(), target).await {
            Ok(resp) => {
                let _ = state
                    .storage
                    .cache_remote_followers(&pubkey, &resp.followers, resp.hidden)
                    .await;
                return Ok(RemoteFollowersResult {
                    followers: resp.followers,
                    hidden: resp.hidden,
                    cached: false,
                });
            }
            Err(e) => {
                log::debug!("[remote-followers] failed via {}: {e}", short_id(node_id));
            }
        }
    }

    if let Some(c) = cached {
        return Ok(RemoteFollowersResult {
            followers: c.entries,
            hidden: c.hidden,
            cached: true,
        });
    }

    Err("peer offline and no cached data".into())
}
