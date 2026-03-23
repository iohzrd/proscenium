use crate::error::CmdResult;
use crate::state::AppState;
use crate::util::generate_id;
use iroh::SecretKey;
use proscenium_types::{
    Interaction, InteractionKind, PostCounts, now_millis, sign_delete_interaction, sign_interaction,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn like_post(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    target_author: String,
) -> CmdResult<Interaction> {
    let (my_id, signing_key_bytes) = {
        let id = state.identity.read().await;
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
    state.storage.save_interaction(&interaction).await?;
    state.gossip().broadcast_interaction(&interaction).await?;
    Ok(interaction)
}

#[tauri::command]
pub async fn unlike_post(state: State<'_, Arc<AppState>>, target_post_id: String) -> CmdResult<()> {
    let (my_id, signing_key_bytes) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.signing_secret_key_bytes)
    };
    let id = state
        .storage
        .delete_interaction_by_target(&my_id, "Like", &target_post_id)
        .await?;
    if let Some(id) = id {
        let signature =
            sign_delete_interaction(&id, &my_id, &SecretKey::from_bytes(&signing_key_bytes));
        state
            .gossip()
            .broadcast_delete_interaction(&id, &my_id, &signature)
            .await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_post_counts(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> CmdResult<PostCounts> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    let counts = state
        .storage
        .get_post_counts(&my_id, &target_post_id)
        .await?;
    Ok(counts)
}
