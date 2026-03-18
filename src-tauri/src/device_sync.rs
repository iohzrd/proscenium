use crate::constants::BATCH_SIZE;
use crate::error::AppError;
use crate::framing::{read_frame, write_frame};
use crate::storage::Storage;
use iroh::protocol::AcceptError;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, endpoint::Connection};
use proscenium_types::{
    DeviceSyncFrame, DeviceSyncVector, PEER_ALPN, PeerRequest, PeerResponse, short_id,
    sign_device_sync_challenge, verify_device_sync_challenge,
};
use std::collections::HashSet;
use std::sync::Arc;

/// Handle an incoming DeviceSyncRequest from another linked device.
/// Verifies the challenge, sends our vector + deltas, then imports the initiator's deltas.
#[allow(clippy::too_many_arguments)]
pub async fn handle_device_sync(
    storage: &Storage,
    master_pubkey: &str,
    signing_secret_key_bytes: &[u8; 32],
    mut send: iroh::endpoint::SendStream,
    challenge: Vec<u8>,
    challenge_sig: String,
    peer_vector: DeviceSyncVector,
    conn: &Connection,
) -> Result<(), AcceptError> {
    let map_err = |e: AppError| AcceptError::from_err(e);

    // Verify the initiator's challenge signature (proves they have the signing key)
    let signing_secret = SecretKey::from_bytes(signing_secret_key_bytes);
    let signing_pubkey = signing_secret.public();

    if let Err(reason) = verify_device_sync_challenge(&challenge, &challenge_sig, &signing_pubkey) {
        log::warn!("[device-sync] challenge verification failed: {reason}");
        return Err(AcceptError::from_err(std::io::Error::other(
            "challenge verification failed",
        )));
    }

    // Build our own vector
    let our_vector = storage
        .build_device_sync_vector(master_pubkey)
        .await
        .map_err(map_err)?;

    // Sign the challenge back to prove we also have the signing key
    let challenge_response = sign_device_sync_challenge(&challenge, &signing_secret);

    // Send our acceptance + vector
    let response = PeerResponse::DeviceSyncAccepted {
        challenge_response,
        vector: our_vector.clone(),
    };
    let resp_bytes = serde_json::to_vec(&response).map_err(AcceptError::from_err)?;
    send.write_all(&resp_bytes)
        .await
        .map_err(AcceptError::from_err)?;
    send.finish().map_err(AcceptError::from_err)?;

    // Open a data stream to send our deltas first.
    // Responder must open_bi() because QUIC streams are lazy: the remote
    // won't see the stream via accept_bi() until data or FIN is sent on it.
    let (mut data_send, mut data_recv) = conn.open_bi().await.map_err(AcceptError::from_err)?;

    // Compute and send deltas that the peer needs
    send_deltas(
        storage,
        master_pubkey,
        our_vector,
        &peer_vector,
        &mut data_send,
    )
    .await
    .map_err(|e| AcceptError::from_err(e))?;

    // End-of-stream marker
    write_frame(&mut data_send, &[])
        .await
        .map_err(|e| AcceptError::from_err(e))?;
    data_send.finish().map_err(AcceptError::from_err)?;

    // Receive and import deltas from the initiator
    import_deltas(storage, master_pubkey, &mut data_recv)
        .await
        .map_err(|e| AcceptError::from_err(e))?;

    log::info!("[device-sync] completed sync with peer device");
    conn.closed().await;
    Ok(())
}

/// Client: initiate a device sync with another linked device.
pub async fn sync_with_device(
    endpoint: &Endpoint,
    storage: &Arc<Storage>,
    target: EndpointId,
    master_pubkey: &str,
    signing_secret_key_bytes: &[u8; 32],
) -> Result<DeviceSyncStats, AppError> {
    let signing_secret = SecretKey::from_bytes(signing_secret_key_bytes);
    let signing_pubkey = signing_secret.public();

    // Generate challenge
    let mut challenge = [0u8; 32];
    getrandom::fill(&mut challenge)?;
    let challenge_sig = sign_device_sync_challenge(&challenge, &signing_secret);

    // Build our sync vector
    let our_vector = storage.build_device_sync_vector(master_pubkey).await?;

    // Connect and send request
    let addr = EndpointAddr::from(target);
    let conn = endpoint.connect(addr, PEER_ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    let req = PeerRequest::DeviceSyncRequest {
        challenge: challenge.to_vec(),
        challenge_sig,
        vector: our_vector.clone(),
    };
    let req_bytes = serde_json::to_vec(&req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    // Read response
    let resp_bytes = recv.read_to_end(1_000_000).await?;
    let response: PeerResponse = serde_json::from_slice(&resp_bytes)?;

    let (challenge_response, peer_vector) = match response {
        PeerResponse::DeviceSyncAccepted {
            challenge_response,
            vector,
        } => (challenge_response, vector),
        _ => {
            return Err(AppError::Other(
                "unexpected response type for device sync".into(),
            ));
        }
    };

    // Verify the responder's challenge response
    if let Err(reason) =
        verify_device_sync_challenge(&challenge, &challenge_response, &signing_pubkey)
    {
        return Err(AppError::Other(format!("peer failed challenge: {reason}")));
    }

    // Accept the data stream opened by the responder.
    // The responder opens this stream and sends first (QUIC streams are lazy).
    let (mut data_send, mut data_recv) = conn.accept_bi().await?;

    // Receive and import deltas from the responder
    let imported = import_deltas(storage, master_pubkey, &mut data_recv).await?;

    // Compute and send our deltas
    send_deltas(
        storage,
        master_pubkey,
        our_vector,
        &peer_vector,
        &mut data_send,
    )
    .await?;

    // End-of-stream marker
    write_frame(&mut data_send, &[]).await?;
    data_send.finish()?;

    conn.close(0u32.into(), b"done");

    Ok(imported)
}

/// Stats from a device sync operation.
#[derive(Debug, Default)]
pub struct DeviceSyncStats {
    pub posts_imported: u32,
    pub interactions_imported: u32,
    pub follows_merged: u32,
    pub mutes_merged: u32,
    pub blocks_merged: u32,
    pub bookmarks_added: u32,
    pub ratchets_merged: u32,
}

/// Compute and send deltas that the peer needs.
/// Uses the pre-built `our_vector` to avoid redundant DB queries.
async fn send_deltas(
    storage: &Storage,
    master_pubkey: &str,
    our_vector: DeviceSyncVector,
    peer_vector: &DeviceSyncVector,
    data_send: &mut iroh::endpoint::SendStream,
) -> Result<(), AppError> {
    // Posts: stream posts with timestamp > peer's newest
    let post_after_ts = if peer_vector.newest_post_ts > 0 || peer_vector.post_count == 0 {
        peer_vector.newest_post_ts
    } else {
        // Peer has posts but no newest timestamp -- skip
        u64::MAX
    };
    if post_after_ts < u64::MAX {
        let mut offset = 0;
        loop {
            let batch = storage
                .get_posts_after(master_pubkey, post_after_ts, BATCH_SIZE, offset)
                .await?;
            if batch.is_empty() {
                break;
            }
            offset += batch.len();
            let frame = DeviceSyncFrame::Posts(batch);
            let frame_bytes = serde_json::to_vec(&frame)?;
            write_frame(data_send, &frame_bytes).await?;
        }
    }

    // Interactions: stream after peer's newest
    let interaction_after_ts =
        if peer_vector.newest_interaction_ts > 0 || peer_vector.interaction_count == 0 {
            peer_vector.newest_interaction_ts
        } else {
            u64::MAX
        };
    if interaction_after_ts < u64::MAX {
        let mut offset = 0;
        loop {
            let batch = if interaction_after_ts > 0 {
                storage
                    .get_interactions_after(master_pubkey, interaction_after_ts, BATCH_SIZE, offset)
                    .await?
            } else {
                storage
                    .get_interactions_paged(master_pubkey, BATCH_SIZE, offset)
                    .await?
            };
            if batch.is_empty() {
                break;
            }
            offset += batch.len();
            let frame = DeviceSyncFrame::Interactions(batch);
            let frame_bytes = serde_json::to_vec(&frame)?;
            write_frame(data_send, &frame_bytes).await?;
        }
    }

    // Follows: send entries that are newer than what peer has
    let follow_deltas: Vec<_> = our_vector
        .follows
        .into_iter()
        .filter(|f| {
            if let Some(peer_entry) = peer_vector.follows.iter().find(|pf| pf.pubkey == f.pubkey) {
                f.last_changed_at > peer_entry.last_changed_at
            } else {
                true
            }
        })
        .collect();
    if !follow_deltas.is_empty() {
        let frame = DeviceSyncFrame::Follows(follow_deltas);
        let frame_bytes = serde_json::to_vec(&frame)?;
        write_frame(data_send, &frame_bytes).await?;
    }

    // Mutes: send newer entries
    let mute_deltas: Vec<_> = our_vector
        .mutes
        .into_iter()
        .filter(|m| {
            if let Some(peer_entry) = peer_vector.mutes.iter().find(|pm| pm.pubkey == m.pubkey) {
                m.last_changed_at > peer_entry.last_changed_at
            } else {
                true
            }
        })
        .collect();
    if !mute_deltas.is_empty() {
        let frame = DeviceSyncFrame::Mutes(mute_deltas);
        let frame_bytes = serde_json::to_vec(&frame)?;
        write_frame(data_send, &frame_bytes).await?;
    }

    // Blocks: send newer entries
    let block_deltas: Vec<_> = our_vector
        .blocks
        .into_iter()
        .filter(|b| {
            if let Some(peer_entry) = peer_vector.blocks.iter().find(|pb| pb.pubkey == b.pubkey) {
                b.last_changed_at > peer_entry.last_changed_at
            } else {
                true
            }
        })
        .collect();
    if !block_deltas.is_empty() {
        let frame = DeviceSyncFrame::Blocks(block_deltas);
        let frame_bytes = serde_json::to_vec(&frame)?;
        write_frame(data_send, &frame_bytes).await?;
    }

    // Bookmarks: send ones the peer doesn't have
    let peer_bookmarks: HashSet<&str> = peer_vector.bookmarks.iter().map(|b| b.as_str()).collect();
    let bookmark_deltas: Vec<String> = our_vector
        .bookmarks
        .into_iter()
        .filter(|b| !peer_bookmarks.contains(b.as_str()))
        .collect();
    if !bookmark_deltas.is_empty() {
        let frame = DeviceSyncFrame::Bookmarks(bookmark_deltas);
        let frame_bytes = serde_json::to_vec(&frame)?;
        write_frame(data_send, &frame_bytes).await?;
    }

    // Ratchet sessions: send sessions with updated_at newer than what peer reports
    let peer_ratchet_map: std::collections::HashMap<&str, u64> = peer_vector
        .ratchet_summaries
        .iter()
        .map(|r| (r.peer_pubkey.as_str(), r.updated_at))
        .collect();
    let our_ratchets = storage.export_ratchet_sessions().await?;
    let ratchet_deltas: Vec<_> = our_ratchets
        .into_iter()
        .filter(|r| match peer_ratchet_map.get(r.peer_pubkey.as_str()) {
            Some(&peer_ts) => r.updated_at > peer_ts,
            None => true,
        })
        .collect();
    if !ratchet_deltas.is_empty() {
        let frame = DeviceSyncFrame::RatchetSessions(ratchet_deltas);
        let frame_bytes = serde_json::to_vec(&frame)?;
        write_frame(data_send, &frame_bytes).await?;
    }

    Ok(())
}

/// Import deltas received from a peer device.
async fn import_deltas(
    storage: &Storage,
    master_pubkey: &str,
    data_recv: &mut iroh::endpoint::RecvStream,
) -> Result<DeviceSyncStats, AppError> {
    let mut stats = DeviceSyncStats::default();

    loop {
        let buf = match read_frame(data_recv).await? {
            Some(buf) => buf,
            None => break,
        };

        let frame: DeviceSyncFrame = serde_json::from_slice(&buf)?;
        match frame {
            DeviceSyncFrame::Posts(posts) => {
                for post in &posts {
                    if post.author != master_pubkey {
                        continue;
                    }
                    storage.insert_post(post).await?;
                    stats.posts_imported += 1;
                }
            }
            DeviceSyncFrame::Interactions(interactions) => {
                for interaction in &interactions {
                    if interaction.author != master_pubkey {
                        continue;
                    }
                    storage.save_interaction(interaction).await?;
                    stats.interactions_imported += 1;
                }
            }
            DeviceSyncFrame::Follows(entries) => {
                stats.follows_merged += storage.merge_follows_lww(&entries).await?;
            }
            DeviceSyncFrame::Mutes(entries) => {
                stats.mutes_merged += storage.merge_mutes_lww(&entries).await?;
            }
            DeviceSyncFrame::Blocks(entries) => {
                stats.blocks_merged += storage.merge_blocks_lww(&entries).await?;
            }
            DeviceSyncFrame::Bookmarks(ids) => {
                stats.bookmarks_added += storage.merge_bookmarks(&ids).await?;
            }
            DeviceSyncFrame::RatchetSessions(sessions) => {
                stats.ratchets_merged += storage.merge_ratchet_sessions_lww(&sessions).await?;
            }
        }
    }

    Ok(stats)
}

/// Run a full device sync cycle: sync with all other linked devices.
pub async fn sync_all_devices(
    endpoint: &Endpoint,
    storage: &Arc<Storage>,
    master_pubkey: &str,
    signing_secret_key_bytes: &[u8; 32],
) {
    let device_ids = match storage.get_other_device_node_ids().await {
        Ok(ids) => ids,
        Err(e) => {
            log::error!("[device-sync] failed to get device list: {e}");
            return;
        }
    };

    if device_ids.is_empty() {
        return;
    }

    for node_id_str in &device_ids {
        let target: EndpointId = match node_id_str.parse() {
            Ok(t) => t,
            Err(e) => {
                log::warn!(
                    "[device-sync] invalid node id {}: {e}",
                    short_id(node_id_str)
                );
                continue;
            }
        };

        log::info!(
            "[device-sync] syncing with device {}...",
            short_id(node_id_str)
        );
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            sync_with_device(
                endpoint,
                storage,
                target,
                master_pubkey,
                signing_secret_key_bytes,
            ),
        )
        .await
        {
            Ok(Ok(stats)) => {
                log::info!(
                    "[device-sync] synced with {}: posts={}, interactions={}, follows={}, mutes={}, blocks={}, bookmarks={}, ratchets={}",
                    short_id(node_id_str),
                    stats.posts_imported,
                    stats.interactions_imported,
                    stats.follows_merged,
                    stats.mutes_merged,
                    stats.blocks_merged,
                    stats.bookmarks_added,
                    stats.ratchets_merged,
                );
            }
            Ok(Err(e)) => {
                log::error!(
                    "[device-sync] failed to sync with {}: {e}",
                    short_id(node_id_str)
                );
            }
            Err(_) => {
                log::error!(
                    "[device-sync] timeout syncing with {}",
                    short_id(node_id_str)
                );
            }
        }
    }
}
