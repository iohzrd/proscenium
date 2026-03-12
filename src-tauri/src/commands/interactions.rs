use crate::ext::ResultExt;
use crate::state::{AppState, generate_id};
use crate::storage::PostCounts;
use iroh::SecretKey;
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
    let my_id = state.master_pubkey.clone();
    let mut interaction = Interaction {
        id: generate_id(),
        author: my_id,
        kind: InteractionKind::Like,
        target_post_id,
        target_author,
        timestamp: now_millis(),
        signature: String::new(),
    };
    let sk = SecretKey::from_bytes(&state.signing_secret_key_bytes);
    sign_interaction(&mut interaction, &sk);
    state
        .storage
        .save_interaction(&interaction)
        .await
        .str_err()?;
    let feed = state.feed.read().await;
    feed.broadcast_interaction(&interaction).await.str_err()?;
    Ok(interaction)
}

#[tauri::command]
pub async fn unlike_post(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> Result<(), String> {
    let my_id = state.master_pubkey.clone();
    let id = state
        .storage
        .delete_interaction_by_target(&my_id, "Like", &target_post_id)
        .await
        .str_err()?;
    if let Some(id) = id {
        let sk = SecretKey::from_bytes(&state.signing_secret_key_bytes);
        let signature = sign_delete_interaction(&id, &my_id, &sk);
        let feed = state.feed.read().await;
        feed.broadcast_delete_interaction(&id, &my_id, &signature)
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
    let author = state.master_pubkey.clone();
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

    let sk = SecretKey::from_bytes(&state.signing_secret_key_bytes);
    sign_post(&mut post, &sk);

    state.storage.insert_post(&post).await.str_err()?;
    let feed = state.feed.read().await;
    feed.broadcast_post(&post).await.str_err()?;
    Ok(post)
}

#[tauri::command]
pub async fn unrepost(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> Result<(), String> {
    let my_id = state.master_pubkey.clone();
    let id = state
        .storage
        .delete_repost_by_target(&my_id, &target_post_id)
        .await
        .str_err()?;
    if let Some(id) = id {
        let sk = SecretKey::from_bytes(&state.signing_secret_key_bytes);
        let signature = sign_delete_post(&id, &my_id, &sk);
        let feed = state.feed.read().await;
        feed.broadcast_delete(&id, &my_id, &signature)
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
    let my_id = state.master_pubkey.clone();
    state
        .storage
        .get_post_counts(&my_id, &target_post_id)
        .await
        .str_err()
}
