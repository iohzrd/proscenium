use crate::ext::ResultExt;
use crate::state::{AppState, generate_id};
use crate::storage::FeedQuery;
use iroh::SecretKey;
use iroh_social_types::{
    LinkPreview, MediaAttachment, Post, now_millis, sign_delete_post, sign_post, validate_post,
};
use std::sync::Arc;
use tauri::State;

use crate::constants::{DEFAULT_FEED_LIMIT, DEFAULT_REPLY_LIMIT};

#[tauri::command]
pub async fn create_post(
    state: State<'_, Arc<AppState>>,
    content: String,
    media: Option<Vec<MediaAttachment>>,
    reply_to: Option<String>,
    reply_to_author: Option<String>,
    quote_of: Option<String>,
    quote_of_author: Option<String>,
) -> Result<Post, String> {
    // author = master pubkey (permanent identity)
    let author = state.master_pubkey.clone();
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

    // Sign with signing key (not master key)
    let sk = SecretKey::from_bytes(&state.signing_secret_key_bytes);
    sign_post(&mut post, &sk);

    state.storage.insert_post(&post).await.str_err()?;
    log::info!(
        "[post] created post {} ({} media attachments)",
        &post.id,
        media_count
    );
    let feed = state.feed.read().await;
    feed.broadcast_post(&post).await.str_err()?;
    log::info!("[post] broadcast post {}", &post.id);

    Ok(post)
}

#[tauri::command]
pub async fn delete_post(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let my_id = &state.master_pubkey;

    let post = state.storage.get_post_by_id(&id).await.str_err()?;
    match post {
        Some(post) if post.author == *my_id => {}
        Some(_) => {
            return Err("cannot delete posts authored by other users".to_string());
        }
        None => {
            return Err(format!("post {id} not found"));
        }
    }

    // Sign the delete action
    let sk = SecretKey::from_bytes(&state.signing_secret_key_bytes);
    let signature = sign_delete_post(&id, my_id, &sk);

    let removed = state.storage.delete_post(&id).await.str_err()?;
    log::info!("[post] delete post {id}: removed={removed}");
    let feed = state.feed.read().await;
    feed.broadcast_delete(&id, my_id, &signature)
        .await
        .str_err()?;
    log::info!("[post] broadcast delete {id}");

    Ok(())
}

#[tauri::command]
pub async fn get_feed(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    before: Option<u64>,
) -> Result<Vec<Post>, String> {
    let q = FeedQuery {
        limit: limit.unwrap_or(DEFAULT_FEED_LIMIT),
        before,
    };
    let posts = state.storage.get_feed(&q).await.str_err()?;
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
) -> Result<Vec<Post>, String> {
    state
        .storage
        .get_posts_by_author(
            &pubkey,
            limit.unwrap_or(DEFAULT_FEED_LIMIT),
            before,
            media_filter.as_deref(),
        )
        .await
        .str_err()
}

#[tauri::command]
pub async fn get_post(state: State<'_, Arc<AppState>>, id: String) -> Result<Option<Post>, String> {
    state.storage.get_post_by_id(&id).await.str_err()
}

#[tauri::command]
pub async fn get_replies(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    limit: Option<u32>,
    before: Option<u64>,
) -> Result<Vec<Post>, String> {
    state
        .storage
        .get_replies(
            &target_post_id,
            limit.unwrap_or(DEFAULT_REPLY_LIMIT) as usize,
            before,
        )
        .await
        .str_err()
}

#[tauri::command]
pub async fn fetch_link_previews(
    state: State<'_, Arc<AppState>>,
    content: String,
) -> Result<Vec<LinkPreview>, String> {
    let urls = crate::opengraph::extract_urls(&content);
    let mut previews = Vec::new();
    for url in &urls {
        if let Some(preview) = crate::opengraph::get_link_preview(&state.http_client, url).await {
            previews.push(preview);
        }
    }
    Ok(previews)
}
