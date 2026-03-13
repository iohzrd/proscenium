use crate::state::AppState;
use iroh_social_types::{FollowRequestEntry, FollowResponse, now_millis, short_id};
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_follow_requests(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<FollowRequestEntry>, String> {
    state.get_follow_requests().await
}

#[tauri::command]
pub async fn get_pending_follow_request_count(
    state: State<'_, Arc<AppState>>,
) -> Result<u64, String> {
    state.get_pending_follow_request_count().await
}

#[tauri::command]
pub async fn approve_follow_request(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<bool, String> {
    state.approve_follow_request(pubkey).await
}

#[tauri::command]
pub async fn deny_follow_request(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<bool, String> {
    state.deny_follow_request(pubkey).await
}

#[tauri::command]
pub async fn send_follow_request_to_peer(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<String, String> {
    state.send_follow_request_to_peer(pubkey).await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn get_follow_requests(&self) -> Result<Vec<FollowRequestEntry>, String> {
        self.storage
            .get_follow_requests()
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_pending_follow_request_count(&self) -> Result<u64, String> {
        self.storage
            .get_pending_follow_request_count()
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn approve_follow_request(&self, pubkey: String) -> Result<bool, String> {
        log::info!("[follow-req] approving request from {}", short_id(&pubkey));
        let approved = self
            .storage
            .approve_follow_request(&pubkey)
            .await
            .map_err(|e| e.to_string())?;
        if approved {
            let now = now_millis();
            let _ = self.storage.upsert_follower(&pubkey, now).await;
        }
        Ok(approved)
    }

    pub(crate) async fn deny_follow_request(&self, pubkey: String) -> Result<bool, String> {
        log::info!("[follow-req] denying request from {}", short_id(&pubkey));
        self.storage
            .deny_follow_request(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn send_follow_request_to_peer(
        &self,
        pubkey: String,
    ) -> Result<String, String> {
        log::info!(
            "[follow-req] sending follow request to {}",
            short_id(&pubkey)
        );
        let node_ids = self
            .storage
            .get_peer_transport_node_ids(&pubkey)
            .await
            .map_err(|e| e.to_string())?;
        let first_node_id = node_ids
            .first()
            .ok_or_else(|| format!("no transport NodeIds cached for {}", short_id(&pubkey)))?;
        let target: iroh::EndpointId = first_node_id.parse().map_err(|e| format!("{e}"))?;

        let (master_pubkey, signing_secret_key_bytes, delegation) = {
            let id = self.identity.read().await;
            (
                id.master_pubkey.clone(),
                id.signing_secret_key_bytes,
                id.delegation.clone(),
            )
        };

        let response = crate::peer::send_follow_request(
            &self.endpoint,
            &self.storage,
            target,
            &master_pubkey,
            &signing_secret_key_bytes,
            &delegation,
        )
        .await
        .map_err(|e| e.to_string())?;

        let result = match response {
            FollowResponse::Approved(_) => "approved",
            FollowResponse::Denied => "denied",
            FollowResponse::Pending => "pending",
        };
        log::info!("[follow-req] response from {}: {result}", short_id(&pubkey));
        Ok(result.to_string())
    }
}
