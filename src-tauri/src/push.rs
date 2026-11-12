use crate::constants::{
    MAX_PUSH_TOMBSTONES, PEER_CLOSE_WAIT, PEER_RESPONSE_WRITE_TIMEOUT, PUSH_ACK_TIMEOUT,
    PUSH_CONNECT_TIMEOUT,
};
use crate::error::AppError;
use crate::framing::{read_to_end_timeout, wait_closed, write_all_timeout};
use crate::ingest::{
    InteractionOutcome, verify_and_apply_post_tombstones, verify_and_store_interaction,
    verify_and_store_post, verify_and_store_profile,
};
use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, SendStream},
    protocol::AcceptError,
};
use proscenium_types::{
    MAX_PUSH_INTERACTIONS, MAX_PUSH_POSTS, PEER_ALPN, PeerRequest, PushAck, PushMessage,
    Visibility, is_valid_pubkey, short_id, verify_linked_devices_announcement,
};
use tauri::{AppHandle, Emitter};

/// Largest `PushAck` a sender accepts: the ids of `MAX_PUSH_POSTS` posts and
/// `MAX_PUSH_INTERACTIONS` interactions.
const MAX_PUSH_ACK_BYTES: usize = 16 * 1024;

fn reject(reason: &'static str) -> AcceptError {
    AcceptError::from_err(std::io::Error::other(reason))
}

/// Connect to the NodeId `target` and deliver `msg`. Returns the peer's `PushAck`.
///
/// Connect, request write and ack read each have a timeout. An error means the message
/// was not acknowledged; the recipient obtains the content at its next sync.
pub async fn push_to_peer(
    endpoint: &Endpoint,
    target: EndpointId,
    msg: &PushMessage,
) -> Result<PushAck, AppError> {
    let conn = tokio::time::timeout(
        PUSH_CONNECT_TIMEOUT,
        endpoint.connect(EndpointAddr::from(target), PEER_ALPN),
    )
    .await
    .map_err(|_| AppError::Other("push connect timeout".into()))??;

    let (mut send, mut recv) = tokio::time::timeout(PUSH_CONNECT_TIMEOUT, conn.open_bi())
        .await
        .map_err(|_| AppError::Other("push stream open timeout".into()))??;

    let req_bytes = serde_json::to_vec(&PeerRequest::Push(Box::new(msg.clone())))?;
    write_all_timeout(&mut send, &req_bytes, PUSH_ACK_TIMEOUT).await?;

    let ack_bytes = read_to_end_timeout(&mut recv, MAX_PUSH_ACK_BYTES, PUSH_ACK_TIMEOUT).await?;
    let ack: PushAck = serde_json::from_slice(&ack_bytes)?;

    conn.close(0u32.into(), b"done");
    Ok(ack)
}

/// Accept side of a push, dispatched from `PeerHandler::accept` after the
/// `PeerRequest::Push` request was read and parsed.
///
/// Content is accepted only if the local user follows `msg.author` (and, with Private
/// visibility, `msg.author` is also an approved follower), the sending NodeId is in the
/// author's stored device list, and the author signed it.
pub async fn handle_push(
    storage: &Storage,
    my_pubkey: &str,
    remote_str: &str,
    app_handle: &AppHandle,
    mut send: SendStream,
    msg: PushMessage,
    conn: &Connection,
) -> Result<(), AcceptError> {
    if !is_valid_pubkey(&msg.author) || msg.author == my_pubkey {
        return Err(reject("invalid push author"));
    }
    if msg.posts.len() > MAX_PUSH_POSTS
        || msg.interactions.len() > MAX_PUSH_INTERACTIONS
        || msg.tombstones.len() > MAX_PUSH_TOMBSTONES
    {
        log::warn!(
            "[push-rx] oversized push from {}: {} posts, {} interactions, {} tombstones",
            short_id(remote_str),
            msg.posts.len(),
            msg.interactions.len(),
            msg.tombstones.len(),
        );
        return Err(reject("too many items"));
    }
    let author = msg.author.as_str();

    // Content is accepted only from a user the local user follows.
    let visibility = storage
        .get_visibility(my_pubkey)
        .await
        .map_err(AcceptError::from_err)?;
    let allowed = match visibility {
        Visibility::Public | Visibility::Listed => storage.is_following(my_pubkey, author).await,
        Visibility::Private => storage.is_mutual(my_pubkey, author).await,
    }
    .map_err(AcceptError::from_err)?;
    if !allowed {
        log::warn!(
            "[push-rx] rejecting push for {} from {} (not authorized)",
            short_id(author),
            short_id(remote_str)
        );
        return Err(reject("unauthorized"));
    }

    // The author's signed device list is stored before the sender is checked, so a push
    // from a newly linked device is accepted.
    if let Some(announcement) = &msg.announcement {
        if announcement.master_pubkey != author {
            return Err(reject("announcement of another user"));
        }
        match verify_linked_devices_announcement(announcement) {
            Ok(()) => {
                if let Err(e) = storage
                    .cache_peer_announcement(announcement, Some(remote_str))
                    .await
                {
                    log::warn!(
                        "[push-rx] rejected device announcement of {}: {e}",
                        short_id(author)
                    );
                }
            }
            Err(reason) => {
                log::warn!(
                    "[push-rx] invalid device announcement of {}: {reason}",
                    short_id(author)
                );
                return Err(reject("invalid announcement"));
            }
        }
    }

    // The sending NodeId must be one of the author's devices.
    let sender = storage
        .try_get_master_pubkey_for_transport(remote_str)
        .await
        .map_err(AcceptError::from_err)?;
    if sender.as_deref() != Some(author) {
        log::warn!(
            "[push-rx] NodeId {} is not a device of {}",
            short_id(remote_str),
            short_id(author)
        );
        return Err(reject("author mismatch"));
    }

    let mut ack = PushAck {
        received_post_ids: Vec::new(),
        received_interaction_ids: Vec::new(),
    };

    // A muted author is still followed; its content is not stored.
    let hidden = storage
        .is_hidden(author)
        .await
        .map_err(AcceptError::from_err)?;
    if !hidden {
        let removed = verify_and_apply_post_tombstones(storage, author, &msg.tombstones, "push-rx")
            .await
            .posts_removed;

        for post in &msg.posts {
            if post.author == author
                && verify_and_store_post(storage, post, "push-rx", my_pubkey, app_handle).await
            {
                ack.received_post_ids.push(post.id.clone());
            }
        }

        for interaction in &msg.interactions {
            let outcome = verify_and_store_interaction(
                storage,
                interaction,
                author,
                "push-rx",
                my_pubkey,
                app_handle,
            )
            .await;
            if outcome == InteractionOutcome::Stored {
                ack.received_interaction_ids.push(interaction.id.clone());
            }
        }

        if let Some(profile) = &msg.profile {
            match verify_and_store_profile(storage, author, profile).await {
                Ok(true) => {
                    let _ = app_handle.emit("profile-updated", author);
                }
                Ok(false) => {}
                Err(e) => log::warn!("[push-rx] rejected profile from {}: {e}", short_id(author)),
            }
        }

        if removed > 0
            || !ack.received_post_ids.is_empty()
            || !ack.received_interaction_ids.is_empty()
        {
            let _ = app_handle.emit("feed-updated", ());
        }
    }

    log::info!(
        "[push-rx] stored {} posts, {} interactions from {}",
        ack.received_post_ids.len(),
        ack.received_interaction_ids.len(),
        short_id(author)
    );

    let ack_bytes = serde_json::to_vec(&ack).map_err(AcceptError::from_err)?;
    write_all_timeout(&mut send, &ack_bytes, PEER_RESPONSE_WRITE_TIMEOUT)
        .await
        .map_err(AcceptError::from_err)?;

    wait_closed(conn, PEER_CLOSE_WAIT).await;
    Ok(())
}
