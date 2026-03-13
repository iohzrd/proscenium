use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn toggle_bookmark(
    state: State<'_, Arc<AppState>>,
    post_id: String,
) -> Result<bool, String> {
    state.toggle_bookmark(post_id).await
}

#[tauri::command]
pub async fn is_bookmarked(
    state: State<'_, Arc<AppState>>,
    post_id: String,
) -> Result<bool, String> {
    state.is_bookmarked(post_id).await
}

#[tauri::command]
pub async fn mute_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    state.mute_user(pubkey).await
}

#[tauri::command]
pub async fn unmute_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    state.unmute_user(pubkey).await
}

#[tauri::command]
pub async fn is_muted(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<bool, String> {
    state.is_muted(pubkey).await
}

#[tauri::command]
pub async fn get_muted_pubkeys(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    state.get_muted_pubkeys().await
}

#[tauri::command]
pub async fn block_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    state.block_user(pubkey).await
}

#[tauri::command]
pub async fn unblock_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    state.unblock_user(pubkey).await
}

#[tauri::command]
pub async fn is_blocked(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<bool, String> {
    state.is_blocked(pubkey).await
}

#[tauri::command]
pub async fn get_blocked_pubkeys(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    state.get_blocked_pubkeys().await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn toggle_bookmark(&self, post_id: String) -> Result<bool, String> {
        self.storage
            .toggle_bookmark(&post_id)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn is_bookmarked(&self, post_id: String) -> Result<bool, String> {
        self.storage
            .is_bookmarked(&post_id)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn mute_user(&self, pubkey: String) -> Result<(), String> {
        self.storage
            .mute_user(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn unmute_user(&self, pubkey: String) -> Result<(), String> {
        self.storage
            .unmute_user(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn is_muted(&self, pubkey: String) -> Result<bool, String> {
        self.storage
            .is_muted(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_muted_pubkeys(&self) -> Result<Vec<String>, String> {
        self.storage
            .get_muted_pubkeys()
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn block_user(&self, pubkey: String) -> Result<(), String> {
        let is_following = self
            .storage
            .is_following(&pubkey)
            .await
            .map_err(|e| e.to_string())?;
        if is_following {
            self.storage
                .unfollow(&pubkey)
                .await
                .map_err(|e| e.to_string())?;
            self.gossip.unfollow_user(&pubkey).await;
        }
        self.storage
            .block_user(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn unblock_user(&self, pubkey: String) -> Result<(), String> {
        self.storage
            .unblock_user(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn is_blocked(&self, pubkey: String) -> Result<bool, String> {
        self.storage
            .is_blocked(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_blocked_pubkeys(&self) -> Result<Vec<String>, String> {
        self.storage
            .get_blocked_pubkeys()
            .await
            .map_err(|e| e.to_string())
    }
}
