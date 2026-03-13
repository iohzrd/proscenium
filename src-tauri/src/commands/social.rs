use crate::state::{AppState, SyncCommand};
use iroh_social_types::{FollowEntry, FollowerEntry, now_millis, short_id};
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn follow_user(state: State<'_, Arc<AppState>>, node_id: String) -> Result<(), String> {
    state.follow_user(node_id).await
}

#[tauri::command]
pub async fn unfollow_user(state: State<'_, Arc<AppState>>, pubkey: String) -> Result<(), String> {
    state.unfollow_user(pubkey).await
}

#[tauri::command]
pub async fn update_follow_alias(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
    alias: Option<String>,
) -> Result<(), String> {
    state.update_follow_alias(pubkey, alias).await
}

#[tauri::command]
pub async fn get_follows(state: State<'_, Arc<AppState>>) -> Result<Vec<FollowEntry>, String> {
    state.get_follows().await
}

#[tauri::command]
pub async fn get_followers(state: State<'_, Arc<AppState>>) -> Result<Vec<FollowerEntry>, String> {
    state.get_followers().await
}

#[tauri::command]
pub async fn get_peer_node_ids(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<Vec<String>, String> {
    state.get_peer_node_ids(pubkey).await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    async fn resolve_peer_identity(
        &self,
        node_id: &str,
    ) -> Result<iroh_social_types::IdentityResponse, String> {
        let target: iroh::EndpointId = node_id.parse().map_err(|e| format!("{e}"))?;
        match crate::peer::query_identity(&self.endpoint, target).await {
            Ok(identity) => {
                log::info!(
                    "[identity] resolved {} -> master={}",
                    short_id(node_id),
                    short_id(&identity.master_pubkey),
                );
                let _ = self.storage.cache_peer_identity(&identity).await;
                if let Some(profile) = &identity.profile {
                    let _ = self
                        .storage
                        .save_profile(&identity.master_pubkey, profile)
                        .await;
                }
                Ok(identity)
            }
            Err(e) => Err(format!(
                "failed to query identity from {}: {e}",
                short_id(node_id),
            )),
        }
    }

    pub(crate) async fn follow_user(&self, node_id: String) -> Result<(), String> {
        let (my_id, my_transport) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.transport_node_id.clone())
        };
        if node_id == my_transport {
            return Err("cannot follow yourself".to_string());
        }
        log::info!("[follow] resolving identity for {}...", short_id(&node_id));

        let peer_identity = self.resolve_peer_identity(&node_id).await?;
        let pubkey = peer_identity.master_pubkey.clone();

        if pubkey == my_id {
            return Err("cannot follow yourself".to_string());
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
        self.storage
            .follow(&entry)
            .await
            .map_err(|e| e.to_string())?;
        self.gossip
            .follow_user(pubkey.clone(), node_ids.clone())
            .await
            .map_err(|e| e.to_string())?;
        log::info!("[follow] subscribed to gossip for {}", short_id(&pubkey));

        // Queue an immediate background sync for the new followee without blocking the command.
        let _ = self.sync_tx.send(SyncCommand::SyncPeer(pubkey)).await;

        Ok(())
    }

    pub(crate) async fn unfollow_user(&self, pubkey: String) -> Result<(), String> {
        log::info!("[follow] unfollowing {}...", short_id(&pubkey));
        self.storage
            .unfollow(&pubkey)
            .await
            .map_err(|e| e.to_string())?;
        self.gossip.unfollow_user(&pubkey).await;
        let deleted = self
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

    pub(crate) async fn update_follow_alias(
        &self,
        pubkey: String,
        alias: Option<String>,
    ) -> Result<(), String> {
        self.storage
            .update_follow_alias(&pubkey, alias.as_deref())
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_follows(&self) -> Result<Vec<FollowEntry>, String> {
        self.storage.get_follows().await.map_err(|e| e.to_string())
    }

    pub(crate) async fn get_followers(&self) -> Result<Vec<FollowerEntry>, String> {
        self.storage
            .get_followers()
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_peer_node_ids(&self, pubkey: String) -> Result<Vec<String>, String> {
        self.storage
            .get_peer_transport_node_ids(&pubkey)
            .await
            .map_err(|e| e.to_string())
    }
}
