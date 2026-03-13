use crate::state::AppState;
use crate::storage::FeedQuery;
use crate::util::generate_id;
use iroh::SecretKey;
use iroh_social_types::{
    MediaAttachment, Post, now_millis, sign_delete_post, sign_post, validate_post,
};
use std::sync::Arc;
use tauri::State;

use crate::constants::{DEFAULT_FEED_LIMIT, DEFAULT_REPLY_LIMIT};

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn repost(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    target_author: String,
) -> Result<Post, String> {
    state.repost(target_post_id, target_author).await
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
) -> Result<Post, String> {
    state
        .create_post(
            content,
            media,
            reply_to,
            reply_to_author,
            quote_of,
            quote_of_author,
        )
        .await
}

#[tauri::command]
pub async fn unrepost(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
) -> Result<(), String> {
    state.unrepost(target_post_id).await
}

#[tauri::command]
pub async fn delete_post(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    state.delete_post(id).await
}

#[tauri::command]
pub async fn get_feed(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    before: Option<u64>,
) -> Result<Vec<Post>, String> {
    state.get_feed(limit, before).await
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
        .get_user_posts(pubkey, limit, before, media_filter)
        .await
}

#[tauri::command]
pub async fn get_post(state: State<'_, Arc<AppState>>, id: String) -> Result<Option<Post>, String> {
    state.get_post(id).await
}

#[tauri::command]
pub async fn get_replies(
    state: State<'_, Arc<AppState>>,
    target_post_id: String,
    limit: Option<u32>,
    before: Option<u64>,
) -> Result<Vec<Post>, String> {
    state.get_replies(target_post_id, limit, before).await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn repost(
        &self,
        target_post_id: String,
        target_author: String,
    ) -> Result<Post, String> {
        let (author, signing_key_bytes) = {
            let id = self.identity.read().await;
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
        self.storage
            .insert_post(&post)
            .await
            .map_err(|e| e.to_string())?;
        self.gossip
            .broadcast_post(&post)
            .await
            .map_err(|e| e.to_string())?;
        Ok(post)
    }

    pub(crate) async fn unrepost(&self, target_post_id: String) -> Result<(), String> {
        let (my_id, signing_key_bytes) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.signing_secret_key_bytes)
        };
        let id = self
            .storage
            .delete_repost_by_target(&my_id, &target_post_id)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(id) = id {
            let signature =
                sign_delete_post(&id, &my_id, &SecretKey::from_bytes(&signing_key_bytes));
            self.gossip
                .broadcast_delete(&id, &my_id, &signature)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_post(
        &self,
        content: String,
        media: Option<Vec<MediaAttachment>>,
        reply_to: Option<String>,
        reply_to_author: Option<String>,
        quote_of: Option<String>,
        quote_of_author: Option<String>,
    ) -> Result<Post, String> {
        let (author, signing_key_bytes) = {
            let id = self.identity.read().await;
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
        self.storage
            .insert_post(&post)
            .await
            .map_err(|e| e.to_string())?;
        log::info!(
            "[post] created post {} ({} media attachments)",
            &post.id,
            media_count
        );
        self.gossip
            .broadcast_post(&post)
            .await
            .map_err(|e| e.to_string())?;
        log::info!("[post] broadcast post {}", &post.id);
        Ok(post)
    }

    pub(crate) async fn delete_post(&self, id: String) -> Result<(), String> {
        let (my_id, signing_key_bytes) = {
            let ident = self.identity.read().await;
            (ident.master_pubkey.clone(), ident.signing_secret_key_bytes)
        };
        let post = self
            .storage
            .get_post_by_id(&id)
            .await
            .map_err(|e| e.to_string())?;
        match post {
            Some(post) if post.author == my_id => {}
            Some(_) => return Err("cannot delete posts authored by other users".to_string()),
            None => return Err(format!("post {id} not found")),
        }
        let signature = sign_delete_post(&id, &my_id, &SecretKey::from_bytes(&signing_key_bytes));
        let removed = self
            .storage
            .delete_post(&id)
            .await
            .map_err(|e| e.to_string())?;
        log::info!("[post] delete post {id}: removed={removed}");
        self.gossip
            .broadcast_delete(&id, &my_id, &signature)
            .await
            .map_err(|e| e.to_string())?;
        log::info!("[post] broadcast delete {id}");
        Ok(())
    }

    pub(crate) async fn get_feed(
        &self,
        limit: Option<usize>,
        before: Option<u64>,
    ) -> Result<Vec<Post>, String> {
        let q = FeedQuery {
            limit: limit.unwrap_or(DEFAULT_FEED_LIMIT),
            before,
        };
        let posts = self.storage.get_feed(&q).await.map_err(|e| e.to_string())?;
        log::info!("[feed] loaded {} posts", posts.len());
        Ok(posts)
    }

    pub(crate) async fn get_user_posts(
        &self,
        pubkey: String,
        limit: Option<usize>,
        before: Option<u64>,
        media_filter: Option<String>,
    ) -> Result<Vec<Post>, String> {
        self.storage
            .get_posts_by_author(
                &pubkey,
                limit.unwrap_or(DEFAULT_FEED_LIMIT),
                before,
                media_filter.as_deref(),
            )
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_post(&self, id: String) -> Result<Option<Post>, String> {
        self.storage
            .get_post_by_id(&id)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_replies(
        &self,
        target_post_id: String,
        limit: Option<u32>,
        before: Option<u64>,
    ) -> Result<Vec<Post>, String> {
        self.storage
            .get_replies(
                &target_post_id,
                limit.unwrap_or(DEFAULT_REPLY_LIMIT) as usize,
                before,
            )
            .await
            .map_err(|e| e.to_string())
    }
}
