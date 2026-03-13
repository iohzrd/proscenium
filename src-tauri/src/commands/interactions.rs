use crate::ext::ResultExt;
use crate::state::AppState;
use crate::storage::PostCounts;
use crate::util::generate_id;
use iroh_social_types::{
    Interaction, InteractionKind, Post, now_millis, sign_delete_interaction, sign_delete_post,
    sign_interaction, sign_post, validate_post,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn like_post(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    target_author: String,
) -> Result<Interaction, String> {
    let my_id = state.identity.master_pubkey.clone();
    let mut interaction = Interaction {
        id: generate_id(),
        author: my_id,
        kind: InteractionKind::Like,
        target_post_id,
        target_author,
        timestamp: now_millis(),
        signature: String::new(),
    };
    sign_interaction(&mut interaction, &state.identity.signing_key);
    state
        .storage
        .save_interaction(&interaction)
        .await
        .str_err()?;
    state
        .net
        .gossip
        .broadcast_interaction(&interaction)
        .await
        .str_err()?;
    Ok(interaction)
}

#[tauri::command]
pub async fn unlike_post(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> Result<(), String> {
    let my_id = state.identity.master_pubkey.clone();
    let id = state
        .storage
        .delete_interaction_by_target(&my_id, "Like", &target_post_id)
        .await
        .str_err()?;
    if let Some(id) = id {
        let signature = sign_delete_interaction(&id, &my_id, &state.identity.signing_key);
        state
            .net
            .gossip
            .broadcast_delete_interaction(&id, &my_id, &signature)
            .await
            .str_err()?;
    }
    Ok(())
}

#[tauri::command]
pub async fn repost(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    target_author: String,
) -> Result<Post, String> {
    let author = state.identity.master_pubkey.clone();
    let mut post = Post {
        id: generate_id(),
        author,
        content: String::new(),
        timestamp: now_millis(),
        media: vec![],
        reply_to: None,
        reply_to_author: None,
        quote_of: Some(target_post_id),
        quote_of_author: Some(target_author),
        signature: String::new(),
    };

    validate_post(&post)?;

    sign_post(&mut post, &state.identity.signing_key);

    state.storage.insert_post(&post).await.str_err()?;
    state.net.gossip.broadcast_post(&post).await.str_err()?;
    Ok(post)
}

#[tauri::command]
pub async fn unrepost(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> Result<(), String> {
    let my_id = state.identity.master_pubkey.clone();
    let id = state
        .storage
        .delete_repost_by_target(&my_id, &target_post_id)
        .await
        .str_err()?;
    if let Some(id) = id {
        let signature = sign_delete_post(&id, &my_id, &state.identity.signing_key);
        state
            .net
            .gossip
            .broadcast_delete(&id, &my_id, &signature)
            .await
            .str_err()?;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_post_counts(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> Result<PostCounts, String> {
    state
        .storage
        .get_post_counts(&state.identity.master_pubkey, &target_post_id)
        .await
        .str_err()
}
