use crate::error::AppError;
use crate::storage::{Storage, TombstoneOutcome};
use proscenium_types::{
    Interaction, Post, PostTombstone, Profile, StageAnnouncement, StageEnd, is_valid_id,
    now_millis, parse_mentions, short_id, validate_interaction, validate_post, validate_profile,
    verify_delete_post_signature, verify_interaction_signature, verify_post_signature,
    verify_profile_signature, verify_stage_announcement, verify_stage_ended,
};
use tauri::{AppHandle, Emitter, Runtime};

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

/// Validate a post, verify that its author signed it, and insert it unless a tombstone
/// exists for its `(author, id)`.
///
/// Returns `true` only if a new row was inserted. Mention, reply and quote notifications
/// and their events are produced only in that case.
pub async fn verify_and_store_post<R: Runtime>(
    storage: &Storage,
    post: &Post,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle<R>,
) -> bool {
    if let Err(reason) = validate_post(post) {
        log::error!("[{label}] rejected post {}: {reason}", short_id(&post.id));
        return false;
    }
    if let Err(reason) = verify_post_signature(post) {
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

/// Validate an interaction of `expected_author`, verify that its author signed it, and
/// insert it unless a tombstone exists for its `(author, id)`.
///
/// A "like" notification is produced only for `InteractionOutcome::Stored` and only if
/// the liked post is stored locally and authored by `my_id`.
pub async fn verify_and_store_interaction<R: Runtime>(
    storage: &Storage,
    interaction: &Interaction,
    expected_author: &str,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle<R>,
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
    if let Err(reason) = verify_interaction_signature(interaction) {
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

/// Validate a profile of `author`, verify that `author` signed it, and store it under the
/// rule of `Storage::save_profile`.
///
/// Returns `Ok(true)` if the profile row changed.
pub async fn verify_and_store_profile(
    storage: &Storage,
    author: &str,
    profile: &Profile,
) -> Result<bool, AppError> {
    validate_profile(profile)?;
    verify_profile_signature(profile, author)?;
    storage.save_profile(author, profile).await
}

/// Verify signed post deletions by `author`, store them as tombstones and delete the
/// posts they identify.
///
/// Entries with another author, an invalid id or an invalid signature are skipped.
/// Returns the number of post rows deleted and of tombstone rows inserted; both are 0
/// if storing failed.
pub async fn verify_and_apply_post_tombstones(
    storage: &Storage,
    author: &str,
    tombstones: &[PostTombstone],
    label: &str,
) -> TombstoneOutcome {
    let valid: Vec<PostTombstone> = tombstones
        .iter()
        .filter(|t| {
            if t.author != author || !is_valid_id(&t.id) {
                return false;
            }
            match verify_delete_post_signature(&t.id, &t.author, &t.signature) {
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

/// Emit `stage-announced` for a stage announcement received from `author` in a sync
/// summary or a push, only if `author` is its host and signed it.
pub fn emit_verified_stage<R: Runtime>(
    stage: &StageAnnouncement,
    author: &str,
    app_handle: &AppHandle<R>,
    label: &str,
) {
    if stage.host_pubkey != author {
        log::warn!("[{label}] ignored stage announcement with another host");
        return;
    }
    if let Err(reason) = verify_stage_announcement(
        &stage.stage_id,
        &stage.title,
        &stage.ticket,
        &stage.host_pubkey,
        stage.started_at,
        &stage.signature,
    ) {
        log::warn!(
            "[{label}] bad stage announcement from {}: {reason}",
            short_id(author)
        );
        return;
    }
    log::info!(
        "[{label}] live stage {} hosted by {}",
        short_id(&stage.stage_id),
        short_id(author),
    );
    let _ = app_handle.emit(
        "stage-announced",
        serde_json::json!({
            "stage_id": stage.stage_id,
            "title": stage.title,
            "ticket": stage.ticket.to_string(),
            "host_pubkey": stage.host_pubkey,
            "started_at": stage.started_at,
        }),
    );
}

/// Emit `stage-ended-remote` for the end of a stage received from `author` in a push,
/// only if `author` is its host and signed it.
pub fn emit_verified_stage_end<R: Runtime>(
    end: &StageEnd,
    author: &str,
    app_handle: &AppHandle<R>,
    label: &str,
) {
    if end.host_pubkey != author {
        log::warn!("[{label}] ignored stage end with another host");
        return;
    }
    if let Err(reason) = verify_stage_ended(&end.stage_id, &end.host_pubkey, &end.signature) {
        log::warn!(
            "[{label}] bad stage end from {}: {reason}",
            short_id(author)
        );
        return;
    }
    log::info!("[{label}] stage {} ended", short_id(&end.stage_id));
    let _ = app_handle.emit("stage-ended-remote", &end.stage_id);
}

#[cfg(test)]
mod tests {
    //! The stored state after a set of signed records must not depend on the order in
    //! which the records arrive or on how often each arrives.

    use super::*;
    use iroh::SecretKey;
    use proscenium_types::{
        InteractionKind, LinkedDevicesAnnouncement, sign_delete_post, sign_interaction,
        sign_linked_devices_announcement, sign_post, sign_profile,
    };
    use std::collections::HashSet;
    use std::fmt::Debug;
    use std::hash::Hash;

    const LABEL: &str = "test";

    /// Every ordering of `items`.
    fn orderings<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
        if items.len() <= 1 {
            return vec![items.to_vec()];
        }
        let mut result = Vec::new();
        for i in 0..items.len() {
            let mut rest = items.to_vec();
            let first = rest.remove(i);
            for mut tail in orderings(&rest) {
                tail.insert(0, first.clone());
                result.push(tail);
            }
        }
        result
    }

    /// Apply every ordering of `items` to a new storage with `apply`, read the result
    /// with `read`, and require one result for all orderings.
    async fn assert_order_independent<T, S, A, F>(items: &[T], apply: A, read: F)
    where
        T: Clone,
        S: Eq + Hash + Debug,
        A: AsyncFn(&Storage, &T),
        F: AsyncFn(&Storage) -> S,
    {
        let mut results = HashSet::new();
        for order in orderings(items) {
            let storage = Storage::open_in_memory().await.unwrap();
            for item in &order {
                apply(&storage, item).await;
            }
            results.insert(read(&storage).await);
        }
        assert_eq!(results.len(), 1, "{results:?}");
    }

    fn identity(seed: u8) -> (SecretKey, String) {
        let key = SecretKey::from_bytes(&[seed; 32]);
        let pubkey = key.public().to_string();
        (key, pubkey)
    }

    fn post(key: &SecretKey, content: &str, timestamp: u64) -> Post {
        let mut post = Post {
            id: String::new(),
            author: key.public().to_string(),
            content: content.to_string(),
            timestamp,
            media: vec![],
            reply_to: None,
            reply_to_author: None,
            quote_of: None,
            quote_of_author: None,
            signature: String::new(),
        };
        sign_post(&mut post, key);
        post
    }

    #[derive(Clone)]
    enum Item {
        Post(Post),
        PostDeletion(PostTombstone),
        Like(Interaction),
        LikeDeletion(String),
    }

    #[tokio::test]
    async fn posts_and_post_deletions_do_not_depend_on_arrival_order() {
        let app = tauri::test::mock_app();
        let handle = app.handle();
        let (key, author) = identity(1);
        let kept = post(&key, "kept", 1_000);
        let deleted = post(&key, "deleted", 2_000);
        let deletion = PostTombstone {
            id: deleted.id.clone(),
            author: author.clone(),
            signature: sign_delete_post(&deleted.id, &author, &key),
        };
        let items = [
            Item::Post(kept),
            Item::Post(deleted.clone()),
            Item::PostDeletion(deletion),
            Item::Post(deleted),
        ];
        assert_order_independent(
            &items,
            async |storage: &Storage, item: &Item| match item {
                Item::Post(post) => {
                    verify_and_store_post(storage, post, LABEL, "me", handle).await;
                }
                Item::PostDeletion(tombstone) => {
                    verify_and_apply_post_tombstones(
                        storage,
                        &author,
                        std::slice::from_ref(tombstone),
                        LABEL,
                    )
                    .await;
                }
                _ => unreachable!(),
            },
            async |storage: &Storage| {
                let mut posts = storage.get_post_ids_by_author(&author).await.unwrap();
                posts.sort();
                let tombstones: Vec<String> = storage
                    .get_post_tombstones(&author, usize::MAX)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|t| t.id)
                    .collect();
                (posts, tombstones)
            },
        )
        .await;
    }

    #[tokio::test]
    async fn likes_and_like_deletions_do_not_depend_on_arrival_order() {
        let app = tauri::test::mock_app();
        let handle = app.handle();
        let (key, author) = identity(1);
        let (other_key, _) = identity(2);
        let like = |target: &Post, n: u32| {
            let mut like = Interaction {
                id: format!("{n:032x}"),
                author: author.clone(),
                kind: InteractionKind::Like,
                target_post_id: target.id.clone(),
                target_author: target.author.clone(),
                timestamp: 1_000 + u64::from(n),
                signature: String::new(),
            };
            sign_interaction(&mut like, &key);
            like
        };
        let kept = like(&post(&other_key, "a", 1), 1);
        let deleted = like(&post(&other_key, "b", 2), 2);
        let items = [
            Item::Like(kept),
            Item::Like(deleted.clone()),
            Item::LikeDeletion(deleted.id.clone()),
            Item::Like(deleted),
        ];
        assert_order_independent(
            &items,
            async |storage: &Storage, item: &Item| match item {
                Item::Like(like) => {
                    verify_and_store_interaction(storage, like, &author, LABEL, "me", handle).await;
                }
                Item::LikeDeletion(id) => {
                    storage
                        .apply_interaction_deletion(&author, id, 1)
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            },
            async |storage: &Storage| {
                let mut likes: Vec<String> = storage
                    .get_interactions_paged(&author, 100, 0)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|i| i.id)
                    .collect();
                likes.sort();
                likes
            },
        )
        .await;
    }

    #[tokio::test]
    async fn stored_profile_does_not_depend_on_arrival_order() {
        let (key, author) = identity(1);
        let profile = |name: &str, updated_at: u64| {
            let mut profile = Profile {
                display_name: name.to_string(),
                bio: String::new(),
                avatar_hash: None,
                avatar_ticket: None,
                visibility: proscenium_types::Visibility::Public,
                updated_at,
                signature: String::new(),
            };
            sign_profile(&mut profile, &author, &key);
            profile
        };
        // Two profiles with the same `updated_at`, an older one and a duplicate.
        let items = [
            profile("a", 10),
            profile("b", 10),
            profile("old", 9),
            profile("a", 10),
        ];
        assert_order_independent(
            &items,
            async |storage: &Storage, profile: &Profile| {
                verify_and_store_profile(storage, &author, profile)
                    .await
                    .unwrap();
            },
            async |storage: &Storage| {
                let stored = storage.get_profile(&author).await.unwrap().unwrap();
                (stored.display_name, stored.updated_at, stored.signature)
            },
        )
        .await;
    }

    #[tokio::test]
    async fn stored_device_list_does_not_depend_on_arrival_order() {
        let (key, master) = identity(1);
        let node_id = |seed: u8| SecretKey::from_bytes(&[seed; 32]).public().to_string();
        let list = |version: u64, device: u8| {
            let mut list = LinkedDevicesAnnouncement {
                master_pubkey: master.clone(),
                devices: vec![proscenium_types::DeviceEntry {
                    node_id: node_id(device),
                    device_name: "device".to_string(),
                    added_at: 1,
                }],
                removed: vec![],
                version,
                signature: String::new(),
            };
            sign_linked_devices_announcement(&mut list, &key);
            list
        };
        // Two lists with the same version, an older one and a duplicate.
        let items = [list(5, 50), list(5, 60), list(4, 70), list(5, 60)];
        assert_order_independent(
            &items,
            async |storage: &Storage, list: &LinkedDevicesAnnouncement| {
                storage.cache_peer_announcement(list, None).await.unwrap();
            },
            async |storage: &Storage| storage.get_peer_transport_node_ids(&master).await.unwrap(),
        )
        .await;
    }
}
