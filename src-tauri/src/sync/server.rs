use crate::constants::BATCH_SIZE;
use crate::error::AppError;
use crate::framing::write_frame;
use crate::storage::Storage;
use iroh::{endpoint::Connection, protocol::AcceptError};
use proscenium_types::{SyncFrame, SyncMode, SyncRequest, SyncSummary, Visibility, short_id};

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
    stage_handle: &crate::stage::StageActorHandle,
) -> Result<(), AcceptError> {
    // Resolve transport NodeId to master pubkey for access control checks
    let remote_pubkey = storage
        .get_master_pubkey_for_transport(remote_str)
        .await
        .unwrap_or_else(|| remote_str.to_string());

    // Enforce visibility-based sync access control
    let visibility = storage
        .get_visibility(node_id)
        .await
        .unwrap_or(Visibility::Public);
    match visibility {
        Visibility::Public => {} // anyone can sync
        Visibility::Listed => {
            if !storage
                .is_follower(node_id, &remote_pubkey)
                .await
                .unwrap_or(false)
            {
                log::warn!(
                    "[sync-server] rejecting non-follower {} (listed profile)",
                    short_id(&remote_pubkey)
                );
                return Err(AcceptError::from_err(std::io::Error::other("listed")));
            }
        }
        Visibility::Private => {
            if !storage
                .is_mutual(node_id, &remote_pubkey)
                .await
                .unwrap_or(false)
            {
                log::warn!(
                    "[sync-server] rejecting non-mutual {} (private profile)",
                    short_id(&remote_pubkey)
                );
                return Err(AcceptError::from_err(std::io::Error::other("private")));
            }
        }
    }

    let map_err = |e: AppError| AcceptError::from_err(e);

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

    let active_stage = stage_handle.get_active_announcement().await;

    let summary = SyncSummary {
        server_post_count,
        server_interaction_count,
        posts_after_count,
        interactions_after_count,
        mode,
        profile,
        active_stage,
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
        write_frame(&mut data_send, &frame_bytes)
            .await
            .map_err(map_err)?;
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
            write_frame(&mut data_send, &frame_bytes)
                .await
                .map_err(map_err)?;
        }
    }

    // End-of-stream marker
    write_frame(&mut data_send, &[]).await.map_err(map_err)?;
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
