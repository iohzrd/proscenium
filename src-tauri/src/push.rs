use crate::error::AppError;
use crate::ingest::{process_incoming_interaction, process_incoming_post};
use crate::storage::Storage;
use iroh::{Endpoint, EndpointAddr, EndpointId, endpoint::Connection, protocol::AcceptError};
use iroh_social_types::{
    MAX_PUSH_INTERACTIONS, MAX_PUSH_POSTS, PEER_ALPN, PeerRequest, PushAck, PushMessage,
    Visibility, short_id, validate_profile, verify_profile_signature,
};
use tauri::{AppHandle, Emitter};

/// Connect to a peer and deliver a `PushMessage`. Returns the peer's `PushAck`.
/// Caller should treat errors as "peer offline" and not retry — sync is the reliable path.
pub async fn push_to_peer(
    endpoint: &Endpoint,
    target: EndpointId,
    msg: &PushMessage,
) -> Result<PushAck, AppError> {
    let addr = EndpointAddr::from(target);
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        endpoint.connect(addr, PEER_ALPN),
    )
    .await
    .map_err(|_| AppError::Other("push connect timeout".into()))??;

    let (mut send, mut recv) = conn.open_bi().await?;

    let req_bytes = serde_json::to_vec(&PeerRequest::Push(msg.clone()))?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    let ack_bytes =
        tokio::time::timeout(std::time::Duration::from_secs(10), recv.read_to_end(65536))
            .await
            .map_err(|_| AppError::Other("push ack timeout".into()))??;
    let ack: PushAck = serde_json::from_slice(&ack_bytes)?;

    conn.close(0u32.into(), b"done");
    Ok(ack)
}

/// Server-side push handler, dispatched from the unified PeerHandler.
/// The initial PeerRequest::Push has already been read and parsed.
pub async fn handle_push(
    storage: &Storage,
    node_id: &str,
    remote_str: &str,
    app_handle: &AppHandle,
    mut send: iroh::endpoint::SendStream,
    msg: PushMessage,
    conn: &Connection,
) -> Result<(), AcceptError> {
    // Resolve transport NodeId to master pubkey for access control
    let remote_pubkey = storage
        .get_master_pubkey_for_transport(remote_str)
        .await
        .unwrap_or_else(|| remote_str.to_string());

    // Validate author matches the resolved master pubkey
    if msg.author != remote_pubkey {
        log::warn!(
            "[push-rx] author mismatch: msg.author={}, remote={}",
            short_id(&msg.author),
            short_id(&remote_pubkey)
        );
        return Err(AcceptError::from_err(std::io::Error::other(
            "author mismatch",
        )));
    }

    // Verify the sender is someone we expect content from
    let my_visibility = storage
        .get_visibility(node_id)
        .await
        .unwrap_or(Visibility::Public);

    let allowed = match my_visibility {
        Visibility::Public | Visibility::Listed => {
            storage.is_follower(&remote_pubkey).await.unwrap_or(false)
                || storage
                    .get_follows()
                    .await
                    .map(|f| f.iter().any(|e| e.pubkey == remote_pubkey))
                    .unwrap_or(false)
        }
        Visibility::Private => storage.is_mutual(&remote_pubkey).await.unwrap_or(false),
    };

    if !allowed {
        log::warn!(
            "[push-rx] rejecting push from {} (not authorized)",
            short_id(&remote_pubkey)
        );
        return Err(AcceptError::from_err(std::io::Error::other("unauthorized")));
    }

    // Enforce batch size limits
    if msg.posts.len() > MAX_PUSH_POSTS {
        log::warn!(
            "[push-rx] too many posts: {} (max {})",
            msg.posts.len(),
            MAX_PUSH_POSTS
        );
        return Err(AcceptError::from_err(std::io::Error::other(
            "too many posts",
        )));
    }
    if msg.interactions.len() > MAX_PUSH_INTERACTIONS {
        log::warn!(
            "[push-rx] too many interactions: {} (max {})",
            msg.interactions.len(),
            MAX_PUSH_INTERACTIONS
        );
        return Err(AcceptError::from_err(std::io::Error::other(
            "too many interactions",
        )));
    }

    let mut received_post_ids = Vec::new();
    let mut received_interaction_ids = Vec::new();

    // Process posts
    for post in &msg.posts {
        if post.author != remote_pubkey {
            continue;
        }
        if storage.is_hidden(&remote_pubkey).await.unwrap_or(false) {
            continue;
        }
        if process_incoming_post(storage, post, "push-rx", node_id, app_handle).await {
            received_post_ids.push(post.id.clone());
        }
    }

    // Process interactions
    for interaction in &msg.interactions {
        if interaction.author != remote_pubkey {
            continue;
        }
        if storage.is_hidden(&remote_pubkey).await.unwrap_or(false) {
            continue;
        }
        process_incoming_interaction(
            storage,
            interaction,
            &remote_pubkey,
            "push-rx",
            node_id,
            app_handle,
        )
        .await;
        received_interaction_ids.push(interaction.id.clone());
    }

    // Process profile update
    if let Some(profile) = &msg.profile {
        if let Err(reason) = validate_profile(profile) {
            log::error!(
                "[push-rx] rejected profile from {}: {reason}",
                short_id(&remote_pubkey)
            );
        } else {
            // Verify profile signature if we have a cached signing key
            let signer_ok = match storage.get_peer_signing_pubkey(&remote_pubkey).await {
                Ok(Some(signing_pubkey)) => match signing_pubkey.parse::<iroh::PublicKey>() {
                    Ok(pk) => verify_profile_signature(profile, &pk).is_ok(),
                    Err(_) => true, // bad cached key, allow through
                },
                _ => true, // no delegation cached, allow through (backward compat)
            };
            if !signer_ok {
                log::warn!(
                    "[push-rx] bad profile signature from {}",
                    short_id(&remote_pubkey)
                );
            } else if let Err(e) = storage.save_profile(&remote_pubkey, profile).await {
                log::error!("[push-rx] failed to store profile: {e}");
            } else {
                let _ = app_handle.emit("profile-updated", remote_pubkey.as_str());
            }
        }
    }

    if !received_post_ids.is_empty() || !received_interaction_ids.is_empty() {
        let _ = app_handle.emit("feed-updated", ());
    }

    log::info!(
        "[push-rx] processed {} posts, {} interactions from {}",
        received_post_ids.len(),
        received_interaction_ids.len(),
        short_id(&remote_pubkey)
    );

    // Send ack
    let ack = PushAck {
        received_post_ids,
        received_interaction_ids,
    };
    let ack_bytes = serde_json::to_vec(&ack).map_err(AcceptError::from_err)?;
    send.write_all(&ack_bytes)
        .await
        .map_err(AcceptError::from_err)?;
    send.finish().map_err(AcceptError::from_err)?;

    conn.closed().await;
    Ok(())
}
