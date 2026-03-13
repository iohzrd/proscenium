use crate::state::AppState;
use iroh::SecretKey;
use iroh_social_types::{NodeStatus, Profile, Visibility, sign_profile, validate_profile};
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_node_id(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    state.get_node_id().await
}

#[tauri::command]
pub async fn get_pubkey(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    state.get_pubkey().await
}

#[tauri::command]
pub async fn get_my_profile(state: State<'_, Arc<AppState>>) -> Result<Option<Profile>, String> {
    state.get_my_profile().await
}

#[tauri::command]
pub async fn save_my_profile(
    state: State<'_, Arc<AppState>>,
    display_name: String,
    bio: String,
    avatar_hash: Option<String>,
    avatar_ticket: Option<String>,
    visibility: String,
) -> Result<(), String> {
    state
        .save_my_profile(display_name, bio, avatar_hash, avatar_ticket, visibility)
        .await
}

#[tauri::command]
pub async fn get_remote_profile(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<Option<Profile>, String> {
    state.get_remote_profile(pubkey).await
}

#[tauri::command]
pub async fn get_node_status(state: State<'_, Arc<AppState>>) -> Result<NodeStatus, String> {
    state.get_node_status().await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn get_node_id(&self) -> Result<String, String> {
        Ok(self.identity.read().await.transport_node_id.clone())
    }

    pub(crate) async fn get_pubkey(&self) -> Result<String, String> {
        Ok(self.identity.read().await.master_pubkey.clone())
    }

    pub(crate) async fn get_my_profile(&self) -> Result<Option<Profile>, String> {
        let master_pubkey = self.identity.read().await.master_pubkey.clone();
        self.storage
            .get_profile(&master_pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn save_my_profile(
        &self,
        display_name: String,
        bio: String,
        avatar_hash: Option<String>,
        avatar_ticket: Option<String>,
        visibility: String,
    ) -> Result<(), String> {
        let (node_id, signing_key_bytes) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.signing_secret_key_bytes)
        };
        let new_visibility: Visibility = visibility.parse().map_err(|e: String| e)?;
        let mut profile = Profile {
            display_name: display_name.clone(),
            bio: bio.clone(),
            avatar_hash,
            avatar_ticket,
            visibility: new_visibility,
            signature: String::new(),
        };
        validate_profile(&profile)?;
        sign_profile(&mut profile, &SecretKey::from_bytes(&signing_key_bytes));

        let old_visibility = self
            .storage
            .get_visibility(&node_id)
            .await
            .unwrap_or(Visibility::Public);

        if old_visibility != new_visibility {
            self.gossip
                .handle_visibility_change(old_visibility, new_visibility, &profile)
                .await
                .map_err(|e| e.to_string())?;
            log::info!("[profile] visibility transition: {old_visibility} -> {new_visibility}");
        }

        self.storage
            .save_profile(&node_id, &profile)
            .await
            .map_err(|e| e.to_string())?;
        log::info!("[profile] saved profile: {display_name} (visibility={new_visibility})");

        self.gossip
            .broadcast_profile(&profile)
            .await
            .map_err(|e| e.to_string())?;

        if let Ok(servers) = self.storage.get_servers().await {
            for server in servers {
                if server.registered_at.is_some() {
                    let _ = self.sync_profile_to_server_inner(&server.url).await;
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn get_remote_profile(
        &self,
        pubkey: String,
    ) -> Result<Option<Profile>, String> {
        self.storage
            .get_profile(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_node_status(&self) -> Result<NodeStatus, String> {
        let transport_node_id = self.identity.read().await.transport_node_id.clone();
        let addr = self.endpoint.addr();
        let relay_url = addr.relay_urls().next().map(|u| u.to_string());
        let has_relay = relay_url.is_some();
        let follow_count = self.gossip.get_subscription_count().await;
        let follower_count = self
            .storage
            .get_followers()
            .await
            .map(|f| f.len())
            .unwrap_or(0);

        Ok(NodeStatus {
            node_id: transport_node_id,
            has_relay,
            relay_url,
            follow_count,
            follower_count,
        })
    }
}
