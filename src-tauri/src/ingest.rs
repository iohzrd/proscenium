use crate::error::AppError;
use crate::storage::{Storage, TombstoneOutcome};
use iroh::PublicKey;
use proscenium_types::{
    Interaction, Post, PostTombstone, Profile, is_valid_id, now_millis, parse_mentions, short_id,
    validate_interaction, validate_post, validate_profile, verify_delete_post_signature,
    verify_interaction_signature, verify_post_signature, verify_profile_signature,
};
use tauri::{AppHandle, Emitter};

/// Result of offering one interaction to `verify_and_store_interaction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionOutcome {
    /// Wrong author, failed validation, bad signature, tombstoned, or a storage error.
    Rejected,
    /// Valid, and a row for it already existed.
    AlreadyStored,
    /// Valid, and a new row was inserted.
    Stored,
}

/// The signing public key of `master_pubkey` from the delegation cache.
///
/// Returns `None` if no delegation is cached, the cached key does not parse, or the
/// lookup fails. Callers reject the content in that case.
pub async fn resolve_signer(storage: &Storage, master_pubkey: &str) -> Option<PublicKey> {
    match storage.get_peer_signing_pubkey(master_pubkey).await {
        Ok(Some(signing_pubkey)) => signing_pubkey.parse().ok(),
        Ok(None) => None,
        Err(e) => {
            log::error!(
                "[ingest] signing key lookup failed for {}: {e}",
                short_id(master_pubkey)
            );
            None
        }
    }
}

/// Resolve the cached signing key of `post.author`, then run `verify_and_store_post`.
/// Returns `true` if a new post row was inserted.
pub async fn process_incoming_post(
    storage: &Storage,
    post: &Post,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> bool {
    let Some(signer) = resolve_signer(storage, &post.author).await else {
        log::error!(
            "[{label}] rejected post {} (no cached signing key for {})",
            short_id(&post.id),
            short_id(&post.author),
        );
        return false;
    };
    verify_and_store_post(storage, post, &signer, label, my_id, app_handle).await
}

/// Validate a post, verify its signature against `signer`, and insert it unless a
/// tombstone exists for its `(author, id)`.
///
/// Returns `true` only if a new row was inserted. Mention, reply and quote notifications
/// and their events are produced only in that case.
pub async fn verify_and_store_post(
    storage: &Storage,
    post: &Post,
    signer: &PublicKey,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> bool {
    if let Err(reason) = validate_post(post) {
        log::error!("[{label}] rejected post {}: {reason}", short_id(&post.id));
        return false;
    }
    if let Err(reason) = verify_post_signature(post, signer) {
        log::error!(
            "[{label}] rejected post {} (bad sig): {reason}",
            short_id(&post.id)
        );
        return false;
    }
    match storage.is_post_tombstoned(&post.author, &post.id).await {
        Ok(false) => {}
        Ok(true) => {
            log::debug!("[{label}] ignored deleted post {}", short_id(&post.id));
            return false;
        }
        Err(e) => {
            log::error!("[{label}] tombstone lookup failed: {e}");
            return false;
        }
    }
    match storage.insert_post(post).await {
        Ok(true) => {}
        Ok(false) => return false,
        Err(e) => {
            log::error!("[{label}] failed to store post: {e}");
            return false;
        }
    }
    // A tombstone stored between the lookup above and the insert removes the new row.
    match storage.is_post_tombstoned(&post.author, &post.id).await {
        Ok(false) => {}
        Ok(true) => {
            if let Err(e) = storage
                .remove_post_if_tombstoned(&post.author, &post.id)
                .await
            {
                log::error!("[{label}] failed to remove deleted post: {e}");
            }
            return false;
        }
        Err(e) => log::error!("[{label}] tombstone lookup failed: {e}"),
    }

    if post.author != my_id {
        if parse_mentions(&post.content).iter().any(|m| m == my_id) {
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

/// Resolve the cached signing key of `expected_author`, then run
/// `verify_and_store_interaction`. Returns `true` if a new interaction row was inserted.
pub async fn process_incoming_interaction(
    storage: &Storage,
    interaction: &Interaction,
    expected_author: &str,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> bool {
    let Some(signer) = resolve_signer(storage, expected_author).await else {
        log::error!(
            "[{label}] rejected interaction {} (no cached signing key for {})",
            short_id(&interaction.id),
            short_id(expected_author),
        );
        return false;
    };
    verify_and_store_interaction(
        storage,
        interaction,
        expected_author,
        &signer,
        label,
        my_id,
        app_handle,
    )
    .await
        == InteractionOutcome::Stored
}

/// Validate an interaction, verify its signature against `signer`, and insert it unless a
/// tombstone exists for its `(author, id)`.
///
/// A "like" notification is produced only for `InteractionOutcome::Stored` and only if
/// the liked post is stored locally and authored by `my_id`.
pub async fn verify_and_store_interaction(
    storage: &Storage,
    interaction: &Interaction,
    expected_author: &str,
    signer: &PublicKey,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> InteractionOutcome {
    if interaction.author != expected_author {
        return InteractionOutcome::Rejected;
    }
    if let Err(reason) = validate_interaction(interaction) {
        log::error!(
            "[{label}] rejected interaction {}: {reason}",
            short_id(&interaction.id)
        );
        return InteractionOutcome::Rejected;
    }
    if let Err(reason) = verify_interaction_signature(interaction, signer) {
        log::error!(
            "[{label}] rejected interaction {} (bad sig): {reason}",
            short_id(&interaction.id)
        );
        return InteractionOutcome::Rejected;
    }
    match storage
        .is_interaction_tombstoned(&interaction.author, &interaction.id)
        .await
    {
        Ok(false) => {}
        Ok(true) => return InteractionOutcome::Rejected,
        Err(e) => {
            log::error!("[{label}] tombstone lookup failed: {e}");
            return InteractionOutcome::Rejected;
        }
    }
    match storage.save_interaction(interaction).await {
        Ok(true) => {}
        Ok(false) => return InteractionOutcome::AlreadyStored,
        Err(e) => {
            log::error!("[{label}] failed to store interaction: {e}");
            return InteractionOutcome::Rejected;
        }
    }
    // A tombstone stored between the lookup above and the insert removes the new row.
    if let Ok(true) = storage
        .remove_interaction_if_tombstoned(&interaction.author, &interaction.id)
        .await
    {
        return InteractionOutcome::Rejected;
    }

    if interaction.target_author == my_id
        && interaction.author != my_id
        && storage
            .has_post(my_id, &interaction.target_post_id)
            .await
            .unwrap_or(false)
    {
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
    InteractionOutcome::Stored
}

/// Validate a profile of `author`, verify its signature against `signer`, and store it if
/// it differs from the stored profile and is not older.
///
/// Returns `Ok(true)` if the profile row changed.
pub async fn verify_and_store_profile(
    storage: &Storage,
    author: &str,
    profile: &Profile,
    signer: &PublicKey,
) -> Result<bool, AppError> {
    validate_profile(profile)?;
    verify_profile_signature(profile, author, signer)?;
    if let Some(stored) = storage.get_profile(author).await?
        && stored.updated_at == profile.updated_at
        && stored.signature == profile.signature
    {
        return Ok(false);
    }
    storage.save_profile(author, profile).await
}

/// Verify signed post deletions by `author` against `signer`, store them as tombstones
/// and delete the posts they identify.
///
/// Entries with another author, an invalid id or an invalid signature are skipped.
/// Returns the number of post rows deleted and of tombstone rows inserted; both are 0
/// if storing failed.
pub async fn verify_and_apply_post_tombstones(
    storage: &Storage,
    author: &str,
    tombstones: &[PostTombstone],
    signer: &PublicKey,
    label: &str,
) -> TombstoneOutcome {
    let valid: Vec<PostTombstone> = tombstones
        .iter()
        .filter(|t| {
            if t.author != author || !is_valid_id(&t.id) {
                return false;
            }
            match verify_delete_post_signature(&t.id, &t.author, &t.signature, signer) {
                Ok(()) => true,
                Err(reason) => {
                    log::warn!(
                        "[{label}] bad delete-post signature from {}: {reason}",
                        short_id(author)
                    );
                    false
                }
            }
        })
        .cloned()
        .collect();
    if valid.is_empty() {
        return TombstoneOutcome::default();
    }
    match storage
        .apply_post_tombstones(author, &valid, now_millis())
        .await
    {
        Ok(outcome) => outcome,
        Err(e) => {
            log::error!("[{label}] failed to apply post deletions: {e}");
            TombstoneOutcome::default()
        }
    }
}
