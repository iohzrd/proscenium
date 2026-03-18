use crate::storage::Storage;
use iroh::PublicKey;
use proscenium_types::{
    Interaction, Post, parse_mentions, short_id, validate_interaction, validate_post,
    verify_interaction_signature, verify_post_signature,
};
use tauri::{AppHandle, Emitter};

/// Resolve the signing public key for a peer from the delegation cache.
/// Falls back to the master pubkey for backward compat with pre-delegation content.
pub async fn resolve_signer(storage: &Storage, master_pubkey: &str) -> Option<PublicKey> {
    if let Ok(Some(signing_pubkey)) = storage.get_peer_signing_pubkey(master_pubkey).await
        && let Ok(pk) = signing_pubkey.parse()
    {
        return Some(pk);
    }
    master_pubkey.parse().ok()
}

/// Validate, verify signature, and store a single incoming post.
/// Returns `true` if the post was stored successfully.
pub async fn process_incoming_post(
    storage: &Storage,
    post: &Post,
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
pub async fn process_incoming_interaction(
    storage: &Storage,
    interaction: &Interaction,
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
