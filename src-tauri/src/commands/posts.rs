use super::{after_cursor, as_storage_cursor, before_cursor, require_id, require_pubkey};
use crate::constants::{DEFAULT_FEED_LIMIT, DEFAULT_REPLY_LIMIT};
use crate::error::CmdResult;
use crate::state::AppState;
use proscenium_types::{
    MediaAttachment, Post, now_millis, sign_delete_post, sign_post, validate_post,
};
use std::sync::Arc;
use tauri::State;

/// Publish a repost of `target_post_id`: a post with `quote_of` set, no text and no media.
/// Returns an error if the local user already has a repost of that post.
#[tauri::command]
pub async fn repost(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    target_author: String,
) -> CmdResult<Post> {
    require_id(&target_post_id)?;
    require_pubkey(&target_author)?;
    let (author, master_key) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.master_key.clone())
    };
    if state.storage.has_repost(&author, &target_post_id).await? {
        return Err("post is already reposted".into());
    }
    let mut post = Post {
        // Set by `sign_post` from the other fields.
        id: String::new(),
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
    sign_post(&mut post, &master_key);
    validate_post(&post)?;
    state.storage.insert_post(&post).await?;
    state.gossip().broadcast_post(&post).await?;
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
    let (author, master_key) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.master_key.clone())
    };
    let media_count = media.as_ref().map_or(0, |m| m.len());
    let mut post = Post {
        // Set by `sign_post` from the other fields.
        id: String::new(),
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
    sign_post(&mut post, &master_key);
    // Rejects oversized content, invalid media and malformed `reply_to` / `quote_of`
    // ids and authors before anything is stored or sent.
    validate_post(&post)?;
    state.storage.insert_post(&post).await?;
    log::info!(
        "[post] created post {} ({} media attachments)",
        post.id,
        media_count
    );
    state.gossip().broadcast_post(&post).await?;
    log::info!("[post] broadcast post {}", post.id);
    Ok(post)
}

/// Delete the local user's reposts of `target_post_id` and broadcast a delete for each.
/// Quotes of the post (posts with text or media) are kept.
#[tauri::command]
pub async fn unrepost(state: State<'_, Arc<AppState>>, target_post_id: String) -> CmdResult<()> {
    require_id(&target_post_id)?;
    let (my_id, master_key) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.master_key.clone())
    };
    let ids = state
        .storage
        .delete_repost_by_target(&my_id, &target_post_id)
        .await?;
    // Every deleted id is broadcast even if an earlier broadcast failed.
    let mut result = Ok(());
    for id in &ids {
        let signature = sign_delete_post(id, &my_id, &master_key);
        if let Err(e) = state
            .gossip()
            .broadcast_delete(id, &my_id, &signature)
            .await
            && result.is_ok()
        {
            result = Err(e);
        }
    }
    result
}

/// Delete a post of the local user and broadcast the delete.
/// Returns an error if the local user has no stored post with this id.
#[tauri::command]
pub async fn delete_post(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<()> {
    require_id(&id)?;
    let (my_id, master_key) = {
        let ident = state.identity.read().await;
        (ident.master_pubkey.clone(), ident.master_key.clone())
    };
    if !state.storage.delete_post(&my_id, &id).await? {
        return Err(format!("no post {id} by the local user").into());
    }
    log::info!("[post] deleted post {id}");
    let signature = sign_delete_post(&id, &my_id, &master_key);
    state
        .gossip()
        .broadcast_delete(&id, &my_id, &signature)
        .await?;
    log::info!("[post] broadcast delete {id}");
    Ok(())
}

/// Home feed: posts of the local user and of followed authors, newest first.
/// `before` and `before_id` are the timestamp and id of the last post of the previous page.
#[tauri::command]
pub async fn get_feed(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    before: Option<u64>,
    before_id: Option<String>,
) -> CmdResult<Vec<Post>> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    let cursor = before_cursor(before, before_id);
    let posts = state
        .storage
        .get_feed(
            &my_id,
            limit.unwrap_or(DEFAULT_FEED_LIMIT),
            as_storage_cursor(&cursor),
        )
        .await?;
    log::info!("[feed] loaded {} posts", posts.len());
    Ok(posts)
}

/// Posts of `pubkey`, newest first.
/// `before` and `before_id` are the timestamp and id of the last post of the previous page.
#[tauri::command]
pub async fn get_user_posts(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
    limit: Option<usize>,
    before: Option<u64>,
    before_id: Option<String>,
    media_filter: Option<String>,
) -> CmdResult<Vec<Post>> {
    require_pubkey(&pubkey)?;
    let cursor = before_cursor(before, before_id);
    let posts = state
        .storage
        .get_posts_by_author(
            &pubkey,
            limit.unwrap_or(DEFAULT_FEED_LIMIT),
            as_storage_cursor(&cursor),
            media_filter.as_deref(),
        )
        .await?;
    Ok(posts)
}

#[tauri::command]
pub async fn get_post(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<Option<Post>> {
    require_id(&id)?;
    let post = state.storage.get_post_by_id(&id).await?;
    Ok(post)
}

/// Replies to `target_post_id`, oldest first.
/// `after` and `after_id` are the timestamp and id of the last reply of the previous page.
#[tauri::command]
pub async fn get_replies(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    limit: Option<u32>,
    after: Option<u64>,
    after_id: Option<String>,
) -> CmdResult<Vec<Post>> {
    require_id(&target_post_id)?;
    let cursor = after_cursor(after, after_id);
    let replies = state
        .storage
        .get_replies(
            &target_post_id,
            limit.unwrap_or(DEFAULT_REPLY_LIMIT) as usize,
            as_storage_cursor(&cursor),
        )
        .await?;
    Ok(replies)
}
