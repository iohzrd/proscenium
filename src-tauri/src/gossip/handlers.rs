use crate::ingest::{
    InteractionOutcome, verify_and_apply_post_tombstones, verify_and_store_interaction,
    verify_and_store_post, verify_and_store_profile,
};
use crate::storage::Storage;
use bytes::Bytes;
use proscenium_types::{
    GossipMessage, Interaction, LinkedDevicesAnnouncement, Post, PostTombstone, Profile,
    StageTicket, is_valid_id, now_millis, short_id, verify_delete_interaction_signature,
    verify_linked_devices_announcement, verify_stage_announcement, verify_stage_ended,
};
use tauri::{AppHandle, Emitter};

impl super::GossipService {
    /// Handle one message received on the feed topic of the followed user `pk`.
    ///
    /// Every message type must carry `pk` as its author and must be signed by the key `pk`.
    pub(super) async fn handle_follow_message(
        storage: &Storage,
        pk: &str,
        my_id: &str,
        app_handle: &AppHandle,
        content: &Bytes,
    ) {
        log::debug!(
            "[gossip-rx] received {} bytes from {}",
            content.len(),
            short_id(pk)
        );
        match serde_json::from_slice(content) {
            Ok(GossipMessage::NewPost(post)) => {
                Self::handle_new_post(storage, pk, my_id, app_handle, post).await;
            }
            Ok(GossipMessage::DeletePost {
                id,
                author,
                signature,
            }) => {
                Self::handle_delete_post(storage, pk, app_handle, id, author, signature).await;
            }
            Ok(GossipMessage::ProfileUpdate(profile)) => {
                Self::handle_profile_update(storage, pk, app_handle, profile).await;
            }
            Ok(GossipMessage::NewInteraction(interaction)) => {
                Self::handle_new_interaction(storage, pk, my_id, app_handle, interaction).await;
            }
            Ok(GossipMessage::DeleteInteraction {
                id,
                author,
                signature,
            }) => {
                Self::handle_delete_interaction(storage, pk, app_handle, &id, &author, &signature)
                    .await;
            }
            Ok(GossipMessage::LinkedDevices(announcement)) => {
                Self::handle_linked_devices(storage, pk, announcement).await;
            }
            // Not sent by this version; ignored when received.
            Ok(GossipMessage::Heartbeat) => {}
            Ok(GossipMessage::StageAnnouncement {
                stage_id,
                title,
                ticket,
                host_pubkey,
                started_at,
                signature,
            }) => {
                Self::handle_stage_announcement(
                    pk,
                    app_handle,
                    &stage_id,
                    &title,
                    &ticket,
                    &host_pubkey,
                    started_at,
                    &signature,
                );
            }
            Ok(GossipMessage::StageEnded {
                stage_id,
                host_pubkey,
                signature,
            }) => {
                if host_pubkey != pk {
                    return;
                }
                if let Err(reason) = verify_stage_ended(&stage_id, &host_pubkey, &signature) {
                    log::warn!(
                        "[gossip-rx] bad stage-ended from {}: {reason}",
                        short_id(pk)
                    );
                    return;
                }
                log::info!("[gossip-rx] stage ended: {}", short_id(&stage_id));
                let _ = app_handle.emit("stage-ended-remote", &stage_id);
            }
            Err(e) => {
                log::warn!(
                    "[gossip-rx] message from {} does not parse: {e}",
                    short_id(pk)
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_stage_announcement(
        pk: &str,
        app_handle: &AppHandle,
        stage_id: &str,
        title: &str,
        ticket: &StageTicket,
        host_pubkey: &str,
        started_at: u64,
        signature: &str,
    ) {
        // Only the topic owner can announce a stage on this topic.
        if host_pubkey != pk {
            return;
        }
        if let Err(reason) =
            verify_stage_announcement(stage_id, title, ticket, host_pubkey, started_at, signature)
        {
            log::warn!(
                "[gossip-rx] bad stage announcement from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        log::info!("[gossip-rx] stage announcement: {}", short_id(stage_id));
        let _ = app_handle.emit(
            "stage-announced",
            serde_json::json!({
                "stage_id": stage_id,
                "title": title,
                "ticket": ticket.to_string(),
                "host_pubkey": host_pubkey,
                "started_at": started_at,
            }),
        );
    }

    /// True if content of `pk` must not be stored: `pk` is muted or blocked, or the
    /// moderation lookup failed.
    async fn is_hidden_or_unknown(storage: &Storage, pk: &str) -> bool {
        match storage.is_hidden(pk).await {
            Ok(hidden) => hidden,
            Err(e) => {
                log::error!("[gossip-rx] moderation lookup failed: {e}");
                true
            }
        }
    }

    async fn handle_new_post(
        storage: &Storage,
        pk: &str,
        my_id: &str,
        app_handle: &AppHandle,
        post: Post,
    ) {
        if post.author != pk || Self::is_hidden_or_unknown(storage, pk).await {
            return;
        }
        if verify_and_store_post(storage, &post, "gossip-rx", my_id, app_handle).await {
            let _ = app_handle.emit("feed-updated", ());
        }
    }

    async fn handle_delete_post(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        id: String,
        author: String,
        signature: String,
    ) {
        if author != pk {
            return;
        }
        let tombstone = PostTombstone {
            id,
            author,
            signature,
        };
        // Verifies the signature, stores the tombstone and deletes the post row of this
        // author with this id.
        let removed = verify_and_apply_post_tombstones(
            storage,
            pk,
            std::slice::from_ref(&tombstone),
            "gossip-rx",
        )
        .await
        .posts_removed;
        if removed > 0 {
            log::info!(
                "[gossip-rx] deleted post {} of {}",
                short_id(&tombstone.id),
                short_id(pk)
            );
            let _ = app_handle.emit("feed-updated", ());
        }
    }

    async fn handle_profile_update(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        profile: Profile,
    ) {
        match verify_and_store_profile(storage, pk, &profile).await {
            Ok(true) => {
                log::info!("[gossip-rx] stored profile of {}", short_id(pk));
                let _ = app_handle.emit("profile-updated", pk);
            }
            Ok(false) => {}
            Err(e) => log::warn!("[gossip-rx] rejected profile from {}: {e}", short_id(pk)),
        }
    }

    async fn handle_new_interaction(
        storage: &Storage,
        pk: &str,
        my_id: &str,
        app_handle: &AppHandle,
        interaction: Interaction,
    ) {
        if interaction.author != pk || Self::is_hidden_or_unknown(storage, pk).await {
            return;
        }
        if verify_and_store_interaction(storage, &interaction, pk, "gossip-rx", my_id, app_handle)
            .await
            == InteractionOutcome::Stored
        {
            let _ = app_handle.emit("interaction-received", &interaction);
        }
    }

    async fn handle_delete_interaction(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        id: &str,
        author: &str,
        signature: &str,
    ) {
        if author != pk || !is_valid_id(id) {
            return;
        }
        if let Err(reason) = verify_delete_interaction_signature(id, author, signature) {
            log::warn!(
                "[gossip-rx] bad delete-interaction signature from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        // The tombstone is stored first: from then on the interaction is not stored again.
        if let Err(e) = storage
            .record_interaction_tombstone(author, id, now_millis())
            .await
        {
            log::error!("[gossip-rx] failed to store interaction deletion: {e}");
            return;
        }
        match storage.delete_interaction(id, author).await {
            Ok(true) => {
                log::info!(
                    "[gossip-rx] deleted interaction {} of {}",
                    short_id(id),
                    short_id(pk)
                );
                let _ = app_handle.emit(
                    "interaction-deleted",
                    serde_json::json!({ "id": id, "author": author }),
                );
            }
            Ok(false) => {}
            Err(e) => log::error!("[gossip-rx] failed to delete interaction: {e}"),
        }
    }

    async fn handle_linked_devices(
        storage: &Storage,
        pk: &str,
        announcement: LinkedDevicesAnnouncement,
    ) {
        if announcement.master_pubkey != pk {
            log::warn!(
                "[gossip-rx] ignoring device announcement from {} (expected {})",
                short_id(&announcement.master_pubkey),
                short_id(pk)
            );
            return;
        }
        if let Err(reason) = verify_linked_devices_announcement(&announcement) {
            log::warn!(
                "[gossip-rx] bad device announcement from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        match storage.cache_peer_announcement(&announcement).await {
            Ok(crate::storage::AnnouncementOutcome::Stored) => {
                log::info!(
                    "[gossip-rx] device announcement from {} v{} ({} devices)",
                    short_id(pk),
                    announcement.version,
                    announcement.devices.len()
                );
            }
            Ok(crate::storage::AnnouncementOutcome::NotNewer) => {}
            Err(e) => {
                log::warn!(
                    "[gossip-rx] rejected device announcement from {}: {e}",
                    short_id(pk)
                );
            }
        }
    }
}
