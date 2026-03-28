use super::SyncResult;
use crate::ingest::{process_incoming_interaction, process_incoming_post};
use crate::storage::Storage;
use tauri::AppHandle;

/// Validate and store posts/interactions/profile from a sync result.
/// Returns the number of posts actually stored.
pub async fn process_sync_result(
    storage: &Storage,
    pubkey: &str,
    result: &SyncResult,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> usize {
    let mut stored = 0;
    for post in &result.posts {
        if process_incoming_post(storage, post, label, my_id, app_handle).await {
            stored += 1;
        }
    }
    if let Some(profile) = &result.profile
        && let Err(e) = storage.save_profile(pubkey, profile).await
    {
        log::error!("[{label}] failed to store profile: {e}");
    }
    for interaction in &result.interactions {
        process_incoming_interaction(storage, interaction, pubkey, label, my_id, app_handle).await;
    }
    stored
}
