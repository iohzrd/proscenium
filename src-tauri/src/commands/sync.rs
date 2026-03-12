use crate::ext::ResultExt;
use crate::state::{AppState, FrontendSyncResult, SyncStatus};
use crate::storage::Storage;
use iroh::PublicKey;
use iroh_social_types::{
    parse_mentions, short_id, validate_interaction, validate_post, verify_interaction_signature,
    verify_post_signature,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

/// Resolve the signing public key for a peer from the delegation cache.
/// Falls back to the master pubkey for backward compat with pre-delegation content.
async fn resolve_signer(storage: &Storage, master_pubkey: &str) -> Option<PublicKey> {
    // Try cached signing key first (from peer_delegations)
    if let Ok(Some(signing_pubkey)) = storage.get_peer_signing_pubkey(master_pubkey).await
        && let Ok(pk) = signing_pubkey.parse()
    {
        return Some(pk);
    }
    // Fall back to master pubkey (backward compat: pre-delegation content was signed by master key)
    master_pubkey.parse().ok()
}

/// Validate, verify signature, and store a single incoming post.
/// Returns `true` if the post was stored successfully.
pub(crate) async fn process_incoming_post(
    storage: &Storage,
    post: &iroh_social_types::Post,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> bool {
    if let Err(reason) = validate_post(post) {
        log::error!("[{label}] rejected post {}: {reason}", &post.id);
        return false;
    }
    let signer = match resolve_signer(storage, &post.author).await {
        Some(pk) => pk,
        None => {
            log::error!(
                "[{label}] rejected post {} (cannot resolve signer for {})",
                &post.id,
                short_id(&post.author),
            );
            return false;
        }
    };
    if let Err(reason) = verify_post_signature(post, &signer) {
        log::error!("[{label}] rejected post {} (bad sig): {reason}", &post.id);
        return false;
    }
    if let Err(e) = storage.insert_post(post).await {
        log::error!("[{label}] failed to store post: {e}");
        return false;
    }
    if post.author != my_id {
        if parse_mentions(&post.content).contains(&my_id.to_string()) {
            let _ = storage
                .insert_notification(
                    "mention",
                    &post.author,
                    None,
                    Some(&post.id),
                    post.timestamp,
                )
                .await;
            let _ = app_handle.emit("mentioned-in-post", post);
            let _ = app_handle.emit("notification-received", ());
        }
        if post.reply_to_author.as_deref() == Some(my_id) {
            let _ = storage
                .insert_notification(
                    "reply",
                    &post.author,
                    post.reply_to.as_deref(),
                    Some(&post.id),
                    post.timestamp,
                )
                .await;
            let _ = app_handle.emit("notification-received", ());
        }
        if post.quote_of_author.as_deref() == Some(my_id) {
            let _ = storage
                .insert_notification(
                    "quote",
                    &post.author,
                    post.quote_of.as_deref(),
                    Some(&post.id),
                    post.timestamp,
                )
                .await;
            let _ = app_handle.emit("notification-received", ());
        }
    }
    true
}

/// Validate, verify signature, and store a single incoming interaction.
pub(crate) async fn process_incoming_interaction(
    storage: &Storage,
    interaction: &iroh_social_types::Interaction,
    expected_author: &str,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) {
    if interaction.author != expected_author {
        return;
    }
    if let Err(reason) = validate_interaction(interaction) {
        log::error!(
            "[{label}] rejected interaction {}: {reason}",
            &interaction.id
        );
        return;
    }
    let signer = match resolve_signer(storage, &interaction.author).await {
        Some(pk) => pk,
        None => {
            log::error!(
                "[{label}] rejected interaction {} (cannot resolve signer for {})",
                &interaction.id,
                short_id(&interaction.author),
            );
            return;
        }
    };
    if let Err(reason) = verify_interaction_signature(interaction, &signer) {
        log::error!(
            "[{label}] rejected interaction {} (bad sig): {reason}",
            &interaction.id
        );
        return;
    }
    let _ = storage.save_interaction(interaction).await;
    if interaction.target_author == my_id && interaction.author != my_id {
        let _ = storage
            .insert_notification(
                "like",
                &interaction.author,
                Some(&interaction.target_post_id),
                None,
                interaction.timestamp,
            )
            .await;
        let _ = app_handle.emit("notification-received", ());
    }
}

/// Validate and store posts/interactions/profile from a sync result.
/// Returns the number of posts actually stored.
pub(crate) async fn process_sync_result(
    storage: &Storage,
    pubkey: &str,
    result: &crate::sync::SyncResult,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> usize {
    let mut stored = 0;
    for post in &result.posts {
        if process_incoming_post(storage, post, label, my_id, app_handle).await {
            stored += 1;
        }
    }
    if let Some(profile) = &result.profile
        && let Err(e) = storage.save_profile(pubkey, profile).await
    {
        log::error!("[{label}] failed to store profile: {e}");
    }
    for interaction in &result.interactions {
        process_incoming_interaction(storage, interaction, pubkey, label, my_id, app_handle).await;
    }
    stored
}

#[tauri::command]
pub async fn sync_posts(
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<FrontendSyncResult, String> {
    let endpoint = state.endpoint.clone();
    let storage = state.storage.clone();

    // Look up cached transport NodeIds for this peer's master pubkey
    let node_ids = storage
        .get_peer_transport_node_ids(&pubkey)
        .await
        .str_err()?;
    if node_ids.is_empty() {
        return Err(format!(
            "no cached transport NodeId for {}",
            short_id(&pubkey)
        ));
    }

    // Try each known transport NodeId until one succeeds
    let my_id = state.master_pubkey.clone();
    let mut last_err = String::new();
    for node_id in &node_ids {
        let target: iroh::EndpointId = match node_id.parse() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("[sync] bad transport NodeId {}: {e}", short_id(node_id));
                continue;
            }
        };
        match crate::sync::sync_from_peer(&endpoint, &storage, target, &pubkey).await {
            Ok(result) => {
                let stored =
                    process_sync_result(&storage, &pubkey, &result, "sync", &my_id, &app_handle)
                        .await;
                log::info!(
                    "[sync] stored {stored}/{} posts from {} via {} (mode={:?})",
                    result.posts.len(),
                    short_id(&pubkey),
                    short_id(node_id),
                    result.mode,
                );
                return Ok(FrontendSyncResult {
                    posts: result.posts,
                    remote_total: result.remote_post_count,
                });
            }
            Err(e) => {
                log::warn!("[sync] failed via {}: {e}", short_id(node_id),);
                last_err = e.to_string();
            }
        }
    }

    Err(format!(
        "sync failed for {} (tried {} node(s)): {last_err}",
        short_id(&pubkey),
        node_ids.len(),
    ))
}

#[tauri::command]
pub async fn get_sync_status(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<SyncStatus, String> {
    let local_count = state
        .storage
        .count_posts_by_author(&pubkey)
        .await
        .str_err()?;
    Ok(SyncStatus { local_count })
}

#[tauri::command]
pub async fn fetch_older_posts(
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> Result<FrontendSyncResult, String> {
    sync_posts(app_handle, state, pubkey).await
}
