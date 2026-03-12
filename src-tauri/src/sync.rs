use crate::storage::Storage;
use iroh::{Endpoint, EndpointAddr, EndpointId, endpoint::Connection, protocol::AcceptError};
use iroh_social_types::{
    Interaction, PEER_ALPN, PeerRequest, Post, Profile, SyncFrame, SyncMode, SyncRequest,
    SyncSummary, Visibility, short_id,
};

const BATCH_SIZE: usize = 200;

/// Write a length-prefixed frame: [4-byte big-endian len][payload].
/// A zero-length frame signals end of stream.
async fn write_frame(
    send: &mut iroh::endpoint::SendStream,
    data: &[u8],
) -> Result<(), AcceptError> {
    let len = data.len() as u32;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(AcceptError::from_err)?;
    if !data.is_empty() {
        send.write_all(data).await.map_err(AcceptError::from_err)?;
    }
    Ok(())
}

/// Read a length-prefixed frame. Returns None on zero-length (end of stream).
async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> anyhow::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > 10_000_000 {
        anyhow::bail!("frame too large: {len} bytes");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Server-side sync handler, dispatched from the unified PeerHandler.
/// The initial PeerRequest::Sync has already been read; `send` is the response side
/// of the first bi-stream, and `conn` is available for opening additional streams.
pub async fn handle_sync(
    storage: &Storage,
    node_id: &str,
    remote_str: &str,
    conn: &Connection,
    mut send: iroh::endpoint::SendStream,
    req: SyncRequest,
) -> Result<(), AcceptError> {
    // Enforce visibility-based sync access control
    let visibility = storage
        .get_visibility(node_id)
        .await
        .unwrap_or(Visibility::Public);
    match visibility {
        Visibility::Public => {} // anyone can sync
        Visibility::Listed => {
            if !storage.is_follower(remote_str).await.unwrap_or(false) {
                log::warn!(
                    "[sync-server] rejecting non-follower {} (listed profile)",
                    short_id(remote_str)
                );
                return Err(AcceptError::from_err(std::io::Error::other("listed")));
            }
        }
        Visibility::Private => {
            if !storage.is_mutual(remote_str).await.unwrap_or(false) {
                log::warn!(
                    "[sync-server] rejecting non-mutual {} (private profile)",
                    short_id(remote_str)
                );
                return Err(AcceptError::from_err(std::io::Error::other("private")));
            }
        }
    }

    let map_err = |e: anyhow::Error| AcceptError::from_err(std::io::Error::other(e.to_string()));

    let server_post_count = storage
        .count_posts_by_author(&req.author)
        .await
        .map_err(map_err)?;
    let server_interaction_count = storage
        .count_interactions_by_author(&req.author)
        .await
        .map_err(map_err)?;
    let posts_after_count = if req.newest_timestamp > 0 {
        storage
            .count_posts_after(&req.author, req.newest_timestamp)
            .await
            .map_err(map_err)?
    } else {
        server_post_count
    };
    let interactions_after_count = if req.newest_interaction_timestamp > 0 {
        storage
            .count_interactions_after(&req.author, req.newest_interaction_timestamp)
            .await
            .map_err(map_err)?
    } else {
        server_interaction_count
    };

    // Determine sync mode
    let posts_match = req.post_count == server_post_count;
    let interactions_match = req.interaction_count == server_interaction_count;
    let mode = if posts_match && interactions_match {
        SyncMode::UpToDate
    } else if server_post_count >= req.post_count
        && (server_post_count - req.post_count) == posts_after_count
    {
        SyncMode::TimestampCatchUp
    } else {
        SyncMode::NeedIdDiff
    };

    let profile = storage.get_profile(node_id).await.ok().flatten();

    log::info!(
        "[sync-server] author={}, client=({}/{}/ts={}/its={}), server=({}/{}/after={}/iafter={}), mode={:?}",
        short_id(&req.author),
        req.post_count,
        req.interaction_count,
        req.newest_timestamp,
        req.newest_interaction_timestamp,
        server_post_count,
        server_interaction_count,
        posts_after_count,
        interactions_after_count,
        mode,
    );

    let summary = SyncSummary {
        server_post_count,
        server_interaction_count,
        posts_after_count,
        interactions_after_count,
        mode,
        profile,
    };
    let summary_bytes = serde_json::to_vec(&summary).map_err(AcceptError::from_err)?;

    // Send Phase 1 summary
    send.write_all(&summary_bytes)
        .await
        .map_err(AcceptError::from_err)?;
    send.finish().map_err(AcceptError::from_err)?;

    if mode == SyncMode::UpToDate {
        conn.closed().await;
        return Ok(());
    }

    // Phase 2 or 3: Open a new bi-stream for streaming data.
    let (mut data_send, mut data_recv) = conn.accept_bi().await?;

    let known_ids: Vec<String> = if mode == SyncMode::NeedIdDiff {
        let ids_bytes = data_recv
            .read_to_end(5_000_000)
            .await
            .map_err(AcceptError::from_err)?;
        serde_json::from_slice(&ids_bytes).map_err(AcceptError::from_err)?
    } else {
        Vec::new()
    };

    // Stream posts
    let mut offset = 0;
    let mut total_sent = 0u64;
    loop {
        let batch = match mode {
            SyncMode::TimestampCatchUp => storage
                .get_posts_after(&req.author, req.newest_timestamp, BATCH_SIZE, offset)
                .await
                .map_err(map_err)?,
            SyncMode::NeedIdDiff => storage
                .get_posts_not_in(&req.author, &known_ids, BATCH_SIZE, offset)
                .await
                .map_err(map_err)?,
            SyncMode::UpToDate => break,
        };

        if batch.is_empty() {
            break;
        }

        total_sent += batch.len() as u64;
        offset += batch.len();

        let frame = SyncFrame::Posts(batch);
        let frame_bytes = serde_json::to_vec(&frame).map_err(AcceptError::from_err)?;
        write_frame(&mut data_send, &frame_bytes).await?;
    }

    // Stream interactions
    if !interactions_match {
        let interaction_catchup = server_interaction_count >= req.interaction_count
            && (server_interaction_count - req.interaction_count) == interactions_after_count;

        let mut ioffset = 0;
        loop {
            let batch = if interaction_catchup {
                storage
                    .get_interactions_after(
                        &req.author,
                        req.newest_interaction_timestamp,
                        BATCH_SIZE,
                        ioffset,
                    )
                    .await
                    .map_err(map_err)?
            } else {
                storage
                    .get_interactions_paged(&req.author, BATCH_SIZE, ioffset)
                    .await
                    .map_err(map_err)?
            };

            if batch.is_empty() {
                break;
            }

            ioffset += batch.len();

            let frame = SyncFrame::Interactions(batch);
            let frame_bytes = serde_json::to_vec(&frame).map_err(AcceptError::from_err)?;
            write_frame(&mut data_send, &frame_bytes).await?;
        }
    }

    // End-of-stream marker
    write_frame(&mut data_send, &[]).await?;
    data_send.finish().map_err(AcceptError::from_err)?;

    log::info!(
        "[sync-server] streamed {} posts to {} (mode={:?})",
        total_sent,
        short_id(remote_str),
        mode,
    );

    conn.closed().await;
    Ok(())
}

/// Result returned from a sync operation.
pub struct SyncResult {
    pub posts: Vec<Post>,
    pub interactions: Vec<Interaction>,
    pub profile: Option<Profile>,
    pub remote_post_count: u64,
    pub mode: SyncMode,
}

/// Client: sync posts from a remote peer using the three-phase protocol.
pub async fn sync_from_peer(
    endpoint: &Endpoint,
    storage: &Storage,
    target: EndpointId,
    author: &str,
) -> anyhow::Result<SyncResult> {
    let addr = EndpointAddr::from(target);
    log::info!(
        "[sync-client] connecting to {} for sync...",
        short_id(author)
    );
    let start = std::time::Instant::now();
    let conn = match endpoint.connect(addr, PEER_ALPN).await {
        Ok(c) => {
            log::info!(
                "[sync-client] connected to {} in {:.1}s",
                short_id(author),
                start.elapsed().as_secs_f64(),
            );
            c
        }
        Err(e) => {
            log::error!(
                "[sync-client] failed to connect to {} after {:.1}s: {e:?}",
                short_id(author),
                start.elapsed().as_secs_f64(),
            );
            return Err(e.into());
        }
    };

    // Phase 1: Send PeerRequest::Sync
    let (mut send, mut recv) = conn.open_bi().await?;

    let post_count = storage.count_posts_by_author(author).await.unwrap_or(0);
    let interaction_count = storage
        .count_interactions_by_author(author)
        .await
        .unwrap_or(0);
    let newest_timestamp = storage.newest_post_timestamp(author).await.unwrap_or(0);
    let newest_interaction_timestamp = storage
        .newest_interaction_timestamp(author)
        .await
        .unwrap_or(0);

    let req = SyncRequest {
        author: author.to_string(),
        post_count,
        interaction_count,
        newest_timestamp,
        newest_interaction_timestamp,
    };
    let peer_req = PeerRequest::Sync(req);
    let req_bytes = serde_json::to_vec(&peer_req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    // Read Phase 1 summary
    let summary_bytes = recv.read_to_end(65_536).await?;
    let summary: SyncSummary = serde_json::from_slice(&summary_bytes)?;

    log::info!(
        "[sync-client] {} mode={:?}, remote=({}/{}), local=({}/{})",
        short_id(author),
        summary.mode,
        summary.server_post_count,
        summary.server_interaction_count,
        post_count,
        interaction_count,
    );

    if summary.mode == SyncMode::UpToDate {
        conn.close(0u32.into(), b"done");
        return Ok(SyncResult {
            posts: Vec::new(),
            interactions: Vec::new(),
            profile: summary.profile,
            remote_post_count: summary.server_post_count,
            mode: SyncMode::UpToDate,
        });
    }

    // Phase 2/3: Open data stream
    let (mut data_send, mut data_recv) = conn.open_bi().await?;

    if summary.mode == SyncMode::NeedIdDiff {
        let known_ids = storage
            .get_post_ids_by_author(author)
            .await
            .unwrap_or_default();
        let ids_bytes = serde_json::to_vec(&known_ids)?;
        data_send.write_all(&ids_bytes).await?;
    }
    data_send.finish()?;

    // Read streamed frames
    let mut all_posts = Vec::new();
    let mut all_interactions = Vec::new();
    loop {
        match read_frame(&mut data_recv).await {
            Ok(Some(frame_bytes)) => {
                let frame: SyncFrame = serde_json::from_slice(&frame_bytes)?;
                match frame {
                    SyncFrame::Posts(posts) => all_posts.extend(posts),
                    SyncFrame::Interactions(interactions) => {
                        all_interactions.extend(interactions);
                    }
                    SyncFrame::DeviceAnnouncements(announcements) => {
                        for announcement in announcements {
                            if announcement.master_pubkey != author {
                                continue;
                            }
                            if iroh_social_types::verify_linked_devices_announcement(&announcement)
                                .is_err()
                            {
                                continue;
                            }
                            if let Err(e) = storage
                                .cache_peer_device_announcement(author, &announcement)
                                .await
                            {
                                log::error!(
                                    "[sync-client] failed to cache device announcement: {e}"
                                );
                            }
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                log::error!(
                    "[sync-client] frame read error from {}: {e:?}",
                    short_id(author)
                );
                break;
            }
        }
    }

    log::info!(
        "[sync-client] received {} posts, {} interactions from {} in {:.1}s (mode={:?})",
        all_posts.len(),
        all_interactions.len(),
        short_id(author),
        start.elapsed().as_secs_f64(),
        summary.mode,
    );

    conn.close(0u32.into(), b"done");

    Ok(SyncResult {
        posts: all_posts,
        interactions: all_interactions,
        profile: summary.profile,
        remote_post_count: summary.server_post_count,
        mode: summary.mode,
    })
}
