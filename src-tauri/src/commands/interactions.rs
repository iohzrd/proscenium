use crate::state::AppState;
use crate::util::generate_id;
use iroh::SecretKey;
use iroh_social_types::{
    Interaction, InteractionKind, PostCounts, now_millis, sign_delete_interaction, sign_interaction,
};
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn like_post(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    target_author: String,
) -> Result<Interaction, String> {
    state.like_post(target_post_id, target_author).await
}

#[tauri::command]
pub async fn unlike_post(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> Result<(), String> {
    state.unlike_post(target_post_id).await
}

#[tauri::command]
pub async fn get_post_counts(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> Result<PostCounts, String> {
    state.get_post_counts(target_post_id).await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn like_post(
        &self,
        target_post_id: String,
        target_author: String,
    ) -> Result<Interaction, String> {
        let (my_id, signing_key_bytes) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.signing_secret_key_bytes)
        };
        let mut interaction = Interaction {
            id: generate_id(),
            author: my_id,
            kind: InteractionKind::Like,
            target_post_id,
            target_author,
            timestamp: now_millis(),
            signature: String::new(),
        };
        sign_interaction(&mut interaction, &SecretKey::from_bytes(&signing_key_bytes));
        self.storage
            .save_interaction(&interaction)
            .await
            .map_err(|e| e.to_string())?;
        self.gossip
            .broadcast_interaction(&interaction)
            .await
            .map_err(|e| e.to_string())?;
        Ok(interaction)
    }

    pub(crate) async fn unlike_post(&self, target_post_id: String) -> Result<(), String> {
        let (my_id, signing_key_bytes) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.signing_secret_key_bytes)
        };
        let id = self
            .storage
            .delete_interaction_by_target(&my_id, "Like", &target_post_id)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(id) = id {
            let signature =
                sign_delete_interaction(&id, &my_id, &SecretKey::from_bytes(&signing_key_bytes));
            self.gossip
                .broadcast_delete_interaction(&id, &my_id, &signature)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub(crate) async fn get_post_counts(
        &self,
        target_post_id: String,
    ) -> Result<PostCounts, String> {
        let my_id = self.identity.read().await.master_pubkey.clone();
        self.storage
            .get_post_counts(&my_id, &target_post_id)
            .await
            .map_err(|e| e.to_string())
    }
}
