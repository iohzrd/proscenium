use crate::ingest::{process_incoming_interaction, process_incoming_post};
use crate::storage::Storage;
use bytes::Bytes;
use proscenium_types::{
    GossipMessage, Interaction, LinkedDevicesAnnouncement, Post, Profile, SigningKeyRotation,
    short_id, validate_profile, verify_delete_interaction_signature, verify_delete_post_signature,
    verify_linked_devices_announcement, verify_profile_signature, verify_rotation,
};
use tauri::{AppHandle, Emitter};

impl super::GossipService {
    pub(super) async fn handle_follow_message(
        storage: &Storage,
        pk: &str,
        my_id: &str,
        app_handle: &AppHandle,
        content: &Bytes,
    ) {
        log::info!(
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
                Self::handle_delete_post(storage, pk, app_handle, &id, &author, &signature).await;
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
            Ok(GossipMessage::SigningKeyRotation(rotation)) => {
                Self::handle_signing_key_rotation(storage, pk, rotation).await;
            }
            Ok(GossipMessage::Heartbeat) => {}
            Ok(GossipMessage::StageAnnouncement {
                stage_id,
                title,
                ticket,
                host_pubkey,
                started_at,
            }) => {
                log::info!(
                    "[gossip-rx] stage announcement: {} ({})",
                    title,
                    short_id(&stage_id)
                );
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
            Ok(GossipMessage::StageEnded { stage_id }) => {
                log::info!("[gossip-rx] stage ended: {}", short_id(&stage_id));
                let _ = app_handle.emit("stage-ended-remote", &stage_id);
            }
            Err(e) => {
                log::error!("[gossip-rx] failed to parse message: {e}");
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
        if post.author != pk {
            log::info!(
                "[gossip-rx] ignored post from {} (expected {})",
                short_id(&post.author),
                short_id(pk)
            );
        } else if storage.is_hidden(pk).await.unwrap_or(false) {
            log::info!(
                "[gossip-rx] skipping post from muted/blocked {}",
                short_id(pk)
            );
        } else if process_incoming_post(storage, &post, "gossip-rx", my_id, app_handle).await {
            let _ = app_handle.emit("feed-updated", ());
        }
    }

    async fn handle_delete_post(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        id: &str,
        author: &str,
        signature: &str,
    ) {
        if author != pk {
            return;
        }
        if let Some(signer) = crate::ingest::resolve_signer(storage, pk).await
            && let Err(reason) = verify_delete_post_signature(id, author, signature, &signer)
        {
            log::warn!(
                "[gossip-rx] bad delete-post signature from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        match storage.get_post_by_id(id).await {
            Ok(Some(post)) if post.author == pk => {
                log::info!("[gossip-rx] delete post {id} from {}", short_id(pk));
                if let Err(e) = storage.delete_post(id).await {
                    log::error!("[gossip-rx] failed to delete post: {e}");
                }
                let _ = app_handle.emit("feed-updated", ());
            }
            Ok(Some(_)) => {
                log::error!("[gossip-rx] rejected delete for {id}: author mismatch");
            }
            Ok(None) => {}
            Err(e) => {
                log::error!("[gossip-rx] failed to look up post {id}: {e}");
            }
        }
    }

    async fn handle_profile_update(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        profile: Profile,
    ) {
        if let Err(reason) = validate_profile(&profile) {
            log::error!(
                "[gossip-rx] rejected profile from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        if let Some(signer) = crate::ingest::resolve_signer(storage, pk).await
            && let Err(reason) = verify_profile_signature(&profile, &signer)
        {
            log::warn!(
                "[gossip-rx] bad profile signature from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        log::info!(
            "[gossip-rx] profile update from {}: {}",
            short_id(pk),
            profile.display_name
        );
        if let Err(e) = storage.save_profile(pk, &profile).await {
            log::error!("[gossip-rx] failed to store profile: {e}");
        }
        let _ = app_handle.emit("profile-updated", pk);
    }

    async fn handle_new_interaction(
        storage: &Storage,
        pk: &str,
        my_id: &str,
        app_handle: &AppHandle,
        interaction: Interaction,
    ) {
        if interaction.author != pk {
            return;
        }
        if storage.is_hidden(pk).await.unwrap_or(false) {
            log::info!(
                "[gossip-rx] skipping interaction from muted/blocked {}",
                short_id(pk)
            );
            return;
        }
        process_incoming_interaction(storage, &interaction, pk, "gossip-rx", my_id, app_handle)
            .await;
        let _ = app_handle.emit("interaction-received", &interaction);
    }

    async fn handle_delete_interaction(
        storage: &Storage,
        pk: &str,
        app_handle: &AppHandle,
        id: &str,
        author: &str,
        signature: &str,
    ) {
        if author != pk {
            return;
        }
        if let Some(signer) = crate::ingest::resolve_signer(storage, pk).await
            && let Err(reason) = verify_delete_interaction_signature(id, author, signature, &signer)
        {
            log::warn!(
                "[gossip-rx] bad delete-interaction signature from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        log::info!("[gossip-rx] delete interaction {id} from {}", short_id(pk));
        if let Err(e) = storage.delete_interaction(id, author).await {
            log::error!("[gossip-rx] failed to delete interaction: {e}");
        }
        let _ = app_handle.emit(
            "interaction-deleted",
            serde_json::json!({ "id": id, "author": author }),
        );
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
        let cached_version = storage
            .get_peer_announcement_version(pk)
            .await
            .unwrap_or(None)
            .unwrap_or(0);
        if announcement.version <= cached_version {
            return;
        }
        log::info!(
            "[gossip-rx] device announcement from {} v{} ({} devices)",
            short_id(pk),
            announcement.version,
            announcement.devices.len()
        );
        if let Err(e) = storage
            .cache_peer_device_announcement(pk, &announcement)
            .await
        {
            log::error!("[gossip-rx] failed to cache device announcement: {e}");
        }
    }

    async fn handle_signing_key_rotation(
        storage: &Storage,
        pk: &str,
        rotation: SigningKeyRotation,
    ) {
        if rotation.master_pubkey != pk {
            log::warn!(
                "[gossip-rx] ignoring key rotation from {} (expected {})",
                short_id(&rotation.master_pubkey),
                short_id(pk)
            );
            return;
        }
        if let Err(reason) = verify_rotation(&rotation) {
            log::warn!(
                "[gossip-rx] bad key rotation from {}: {reason}",
                short_id(pk)
            );
            return;
        }
        if let Ok(Some(cached_delegation)) = storage.get_peer_delegation(pk).await
            && rotation.new_key_index <= cached_delegation.key_index
        {
            log::warn!(
                "[gossip-rx] stale key rotation from {} (index {} <= cached {})",
                short_id(pk),
                rotation.new_key_index,
                cached_delegation.key_index
            );
            return;
        }
        log::info!(
            "[gossip-rx] signing key rotation from {} to index {}",
            short_id(pk),
            rotation.new_key_index
        );
        let response = proscenium_types::IdentityResponse {
            master_pubkey: rotation.master_pubkey.clone(),
            delegation: rotation.new_delegation.clone(),
            transport_node_ids: storage
                .get_peer_transport_node_ids(pk)
                .await
                .unwrap_or_default(),
            profile: None,
        };
        if let Err(e) = storage.cache_peer_identity(&response).await {
            log::error!("[gossip-rx] failed to cache rotated delegation: {e}");
        }
    }
}
