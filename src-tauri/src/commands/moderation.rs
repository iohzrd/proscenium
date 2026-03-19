use crate::error::CmdResult;
use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn toggle_bookmark(state: State<'_, Arc<AppState>>, post_id: String) -> CmdResult<bool> {
    let toggled = state.storage.toggle_bookmark(&post_id).await?;
    Ok(toggled)
}

#[tauri::command]
pub async fn is_bookmarked(state: State<'_, Arc<AppState>>, post_id: String) -> CmdResult<bool> {
    let bookmarked = state.storage.is_bookmarked(&post_id).await?;
    Ok(bookmarked)
}

#[tauri::command]
pub async fn mute_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    state.storage.mute_user(&pubkey).await?;
    Ok(())
}

#[tauri::command]
pub async fn unmute_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    state.storage.unmute_user(&pubkey).await?;
    Ok(())
}

#[tauri::command]
pub async fn is_muted(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<bool> {
    let muted = state.storage.is_muted(&pubkey).await?;
    Ok(muted)
}

#[tauri::command]
pub async fn get_muted_pubkeys(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<String>> {
    let pubkeys = state.storage.get_muted_pubkeys().await?;
    Ok(pubkeys)
}

#[tauri::command]
pub async fn block_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    let is_following = state.storage.is_following(&my_id, &pubkey).await?;
    if is_following {
        state.storage.unfollow(&my_id, &pubkey).await?;
        state.gossip.unfollow_user(&pubkey).await;
    }
    state.storage.block_user(&pubkey).await?;
    Ok(())
}

#[tauri::command]
pub async fn unblock_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    state.storage.unblock_user(&pubkey).await?;
    Ok(())
}

#[tauri::command]
pub async fn is_blocked(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<bool> {
    let blocked = state.storage.is_blocked(&pubkey).await?;
    Ok(blocked)
}

#[tauri::command]
pub async fn get_blocked_pubkeys(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<String>> {
    let pubkeys = state.storage.get_blocked_pubkeys().await?;
    Ok(pubkeys)
}
