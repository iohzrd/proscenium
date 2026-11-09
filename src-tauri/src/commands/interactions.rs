use super::{require_id, require_pubkey};
use crate::error::CmdResult;
use crate::state::AppState;
use crate::util::generate_id;
use proscenium_types::{
    Interaction, InteractionKind, PostCounts, now_millis, sign_delete_interaction, sign_interaction,
};
use std::sync::Arc;
use tauri::State;

/// `interactions.kind` value of a like.
const KIND_LIKE: &str = "Like";

/// Like a post and broadcast the like.
///
/// If the local user already has a like for `target_post_id`, that stored interaction is
/// broadcast again and returned, so one target never has two interaction ids.
#[tauri::command]
pub async fn like_post(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    target_author: String,
) -> CmdResult<Interaction> {
    require_id(&target_post_id)?;
    require_pubkey(&target_author)?;
    let (my_id, master_key) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.master_key.clone())
    };
    let existing = state
        .storage
        .get_interaction_by_target(&my_id, KIND_LIKE, &target_post_id)
        .await?;
    let interaction = match existing {
        Some(existing) => existing,
        None => {
            let mut interaction = Interaction {
                id: generate_id(),
                author: my_id.clone(),
                kind: InteractionKind::Like,
                target_post_id: target_post_id.clone(),
                target_author,
                timestamp: now_millis(),
                signature: String::new(),
            };
            sign_interaction(&mut interaction, &master_key);
            if state.storage.save_interaction(&interaction).await? {
                interaction
            } else {
                // A concurrent call stored a like for the same target first.
                state
                    .storage
                    .get_interaction_by_target(&my_id, KIND_LIKE, &target_post_id)
                    .await?
                    .ok_or("like was not stored")?
            }
        }
    };
    state.gossip().broadcast_interaction(&interaction).await?;
    Ok(interaction)
}

#[tauri::command]
pub async fn unlike_post(state: State<'_, Arc<AppState>>, target_post_id: String) -> CmdResult<()> {
    require_id(&target_post_id)?;
    let (my_id, master_key) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.master_key.clone())
    };
    let id = state
        .storage
        .delete_interaction_by_target(&my_id, KIND_LIKE, &target_post_id)
        .await?;
    if let Some(id) = id {
        let signature = sign_delete_interaction(&id, &my_id, &master_key);
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
    require_id(&target_post_id)?;
    let my_id = state.identity.read().await.master_pubkey.clone();
    let counts = state
        .storage
        .get_post_counts(&my_id, &target_post_id)
        .await?;
    Ok(counts)
}
