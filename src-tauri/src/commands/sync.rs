use crate::state::{AppState, SyncCommand};
use iroh_social_types::{FrontendSyncResult, SyncStatus, short_id};
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn sync_posts(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<FrontendSyncResult, String> {
    state.sync_posts(pubkey).await
}

#[tauri::command]
pub async fn get_sync_status(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<SyncStatus, String> {
    state.get_sync_status(pubkey).await
}

#[tauri::command]
pub async fn sync_all_peers(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .sync_tx
        .send(SyncCommand::SyncAll)
        .await
        .map_err(|_| "sync task unavailable".to_string())
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn sync_posts(&self, pubkey: String) -> Result<FrontendSyncResult, String> {
        let my_id = self.identity.read().await.master_pubkey.clone();
        let result = crate::sync::sync_one_peer(
            &self.endpoint,
            &self.storage,
            &pubkey,
            &my_id,
            &self.app_handle,
            "sync",
        )
        .await
        .map_err(|e| format!("sync failed for {}: {e}", short_id(&pubkey)))?;

        Ok(FrontendSyncResult {
            posts: result.posts,
            remote_total: result.remote_post_count,
        })
    }

    pub(crate) async fn get_sync_status(&self, pubkey: String) -> Result<SyncStatus, String> {
        let local_count = self
            .storage
            .count_posts_by_author(&pubkey)
            .await
            .map_err(|e| e.to_string())?;
        Ok(SyncStatus { local_count })
    }
}
