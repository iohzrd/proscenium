use crate::ext::ResultExt;
use crate::state::AppState;
use iroh_social_types::{FollowEntry, FollowerEntry, now_millis, short_id};
use std::sync::Arc;
use tauri::{AppHandle, State};

use super::sync::process_sync_result;

#[tauri::command]
pub async fn follow_user(
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<(), String> {
    let my_id = state.endpoint.id().to_string();
    if pubkey == my_id {
        return Err("cannot follow yourself".to_string());
    }
    log::info!("[follow] following {}...", short_id(&pubkey));
    let entry = FollowEntry {
        pubkey: pubkey.clone(),
        alias: None,
        followed_at: now_millis(),
    };
    state.storage.follow(&entry).str_err()?;

    {
        let mut feed = state.feed.lock().await;
        feed.follow_user(pubkey.clone()).await.str_err()?;
    }
    log::info!("[follow] subscribed to gossip for {}", short_id(&pubkey));

    log::info!("[follow] syncing posts from {}...", short_id(&pubkey));
    let endpoint = state.endpoint.clone();
    let storage = state.storage.clone();
    let target: iroh::EndpointId = pubkey.parse().str_err()?;
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

    Ok(())
}

#[tauri::command]
pub async fn unfollow_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    log::info!("[follow] unfollowing {}...", short_id(&pubkey));
    state.storage.unfollow(&pubkey).str_err()?;
    let mut feed = state.feed.lock().await;
    feed.unfollow_user(&pubkey);
    log::info!("[follow] unfollowed {}", short_id(&pubkey));
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
