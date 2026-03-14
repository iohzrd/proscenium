use crate::constants::{DEFAULT_FEED_LIMIT, DEFAULT_REPLY_LIMIT};
use crate::error::CmdResult;
use crate::state::AppState;
use crate::storage::FeedQuery;
use crate::util::generate_id;
use iroh::SecretKey;
use iroh_social_types::{
    MediaAttachment, Post, now_millis, sign_delete_post, sign_post, validate_post,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn repost(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    target_author: String,
) -> CmdResult<Post> {
    let (author, signing_key_bytes) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.signing_secret_key_bytes)
    };
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
    sign_post(&mut post, &SecretKey::from_bytes(&signing_key_bytes));
    state.storage.insert_post(&post).await?;
    state.gossip.broadcast_post(&post).await?;
    Ok(post)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn create_post(
    state: State<'_, Arc<AppState>>,
    content: String,
    media: Option<Vec<MediaAttachment>>,
    reply_to: Option<String>,
    reply_to_author: Option<String>,
    quote_of: Option<String>,
    quote_of_author: Option<String>,
) -> CmdResult<Post> {
    let (author, signing_key_bytes) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.signing_secret_key_bytes)
    };
    let media_count = media.as_ref().map_or(0, |m| m.len());
    let mut post = Post {
        id: generate_id(),
        author,
        content,
        timestamp: now_millis(),
        media: media.unwrap_or_default(),
        reply_to,
        reply_to_author,
        quote_of,
        quote_of_author,
        signature: String::new(),
    };
    validate_post(&post)?;
    sign_post(&mut post, &SecretKey::from_bytes(&signing_key_bytes));
    state.storage.insert_post(&post).await?;
    log::info!(
        "[post] created post {} ({} media attachments)",
        &post.id,
        media_count
    );
    state.gossip.broadcast_post(&post).await?;
    log::info!("[post] broadcast post {}", &post.id);
    Ok(post)
}

#[tauri::command]
pub async fn unrepost(state: State<'_, Arc<AppState>>, target_post_id: String) -> CmdResult<()> {
    let (my_id, signing_key_bytes) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.signing_secret_key_bytes)
    };
    let id = state
        .storage
        .delete_repost_by_target(&my_id, &target_post_id)
        .await?;
    if let Some(id) = id {
        let signature = sign_delete_post(&id, &my_id, &SecretKey::from_bytes(&signing_key_bytes));
        state
            .gossip
            .broadcast_delete(&id, &my_id, &signature)
            .await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_post(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<()> {
    let (my_id, signing_key_bytes) = {
        let ident = state.identity.read().await;
        (ident.master_pubkey.clone(), ident.signing_secret_key_bytes)
    };
    let post = state.storage.get_post_by_id(&id).await?;
    match post {
        Some(post) if post.author == my_id => {}
        Some(_) => return Err("cannot delete posts authored by other users".into()),
        None => return Err(format!("post {id} not found").into()),
    }
    let signature = sign_delete_post(&id, &my_id, &SecretKey::from_bytes(&signing_key_bytes));
    let removed = state.storage.delete_post(&id).await?;
    log::info!("[post] delete post {id}: removed={removed}");
    state
        .gossip
        .broadcast_delete(&id, &my_id, &signature)
        .await?;
    log::info!("[post] broadcast delete {id}");
    Ok(())
}

#[tauri::command]
pub async fn get_feed(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    before: Option<u64>,
) -> CmdResult<Vec<Post>> {
    let q = FeedQuery {
        limit: limit.unwrap_or(DEFAULT_FEED_LIMIT),
        before,
    };
    let posts = state.storage.get_feed(&q).await?;
    log::info!("[feed] loaded {} posts", posts.len());
    Ok(posts)
}

#[tauri::command]
pub async fn get_user_posts(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
    limit: Option<usize>,
    before: Option<u64>,
    media_filter: Option<String>,
) -> CmdResult<Vec<Post>> {
    let posts = state
        .storage
        .get_posts_by_author(
            &pubkey,
            limit.unwrap_or(DEFAULT_FEED_LIMIT),
            before,
            media_filter.as_deref(),
        )
        .await?;
    Ok(posts)
}

#[tauri::command]
pub async fn get_post(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<Option<Post>> {
    let post = state.storage.get_post_by_id(&id).await?;
    Ok(post)
}

#[tauri::command]
pub async fn get_replies(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    limit: Option<u32>,
    before: Option<u64>,
) -> CmdResult<Vec<Post>> {
    let replies = state
        .storage
        .get_replies(
            &target_post_id,
            limit.unwrap_or(DEFAULT_REPLY_LIMIT) as usize,
            before,
        )
        .await?;
    Ok(replies)
}
