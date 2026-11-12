use super::{require_id, require_pubkey};
use crate::error::CmdResult;
use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn toggle_bookmark(state: State<'_, Arc<AppState>>, post_id: String) -> CmdResult<bool> {
    require_id(&post_id)?;
    let toggled = state.storage.toggle_bookmark(&post_id).await?;
    Ok(toggled)
}

#[tauri::command]
pub async fn is_bookmarked(state: State<'_, Arc<AppState>>, post_id: String) -> CmdResult<bool> {
    require_id(&post_id)?;
    let bookmarked = state.storage.is_bookmarked(&post_id).await?;
    Ok(bookmarked)
}

#[tauri::command]
pub async fn mute_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    require_pubkey(&pubkey)?;
    if pubkey == state.identity.read().await.master_pubkey {
        return Err("cannot mute yourself".into());
    }
    state.storage.mute_user(&pubkey).await?;
    Ok(())
}

#[tauri::command]
pub async fn unmute_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    require_pubkey(&pubkey)?;
    state.storage.unmute_user(&pubkey).await?;
    Ok(())
}

#[tauri::command]
pub async fn is_muted(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<bool> {
    require_pubkey(&pubkey)?;
    let muted = state.storage.is_muted(&pubkey).await?;
    Ok(muted)
}

#[tauri::command]
pub async fn get_muted_pubkeys(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<String>> {
    let pubkeys = state.storage.get_muted_pubkeys().await?;
    Ok(pubkeys)
}

/// Block a user. If the local user follows them, the follow is removed and the gossip
/// subscription to their feed is closed first.
#[tauri::command]
pub async fn block_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    require_pubkey(&pubkey)?;
    let my_id = state.identity.read().await.master_pubkey.clone();
    if pubkey == my_id {
        return Err("cannot block yourself".into());
    }
    let is_following = state.storage.is_following(&my_id, &pubkey).await?;
    if is_following {
        state.storage.unfollow(&my_id, &pubkey).await?;
        state.gossip().unfollow_user(&pubkey).await;
    }
    state.storage.block_user(&pubkey).await?;
    // A blocked user loses an approval as follower, so it no longer receives Listed or
    // Private content after an unblock either, until it is approved again.
    let now = proscenium_types::now_millis();
    state
        .storage
        .revoke_follower(
            &my_id,
            &pubkey,
            now,
            now.saturating_add(crate::constants::FOLLOW_REQUEST_TTL_MS),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn unblock_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    require_pubkey(&pubkey)?;
    state.storage.unblock_user(&pubkey).await?;
    Ok(())
}

#[tauri::command]
pub async fn is_blocked(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<bool> {
    require_pubkey(&pubkey)?;
    let blocked = state.storage.is_blocked(&pubkey).await?;
    Ok(blocked)
}

#[tauri::command]
pub async fn get_blocked_pubkeys(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<String>> {
    let pubkeys = state.storage.get_blocked_pubkeys().await?;
    Ok(pubkeys)
}
