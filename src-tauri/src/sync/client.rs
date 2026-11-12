use super::orchestration::SyncOneResult;
use super::processing::FrameIngest;
use crate::constants::{
    SYNC_CONNECT_TIMEOUT, SYNC_MAX_FRAME_BYTES, SYNC_MAX_SUMMARY_BYTES, SYNC_STEP_TIMEOUT,
};
use crate::error::AppError;
use crate::framing::{read_frame_limited, read_to_end_timeout, write_all_timeout};
use crate::ingest::verify_and_store_profile;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use proscenium_types::{
    MAX_SYNC_IDS, PEER_ALPN, PeerRequest, SyncFrame, SyncMode, SyncRequest, SyncSummary,
    post_ids_digest, short_id, verify_linked_devices_announcement,
};
use tauri::{AppHandle, Emitter, Runtime};
use tokio::time::Instant;

/// The node that answers a sync request for an author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncSource {
    /// A device of the author.
    Author,
    /// Another device of the local user; the author is the local user.
    OwnDevice,
    /// A node of another user that stores the author's posts (see
    /// `sync::server::authorize_mirror_requester`).
    Mirror,
}

/// Requester side of the sync protocol: fetch the content of `author` from the NodeId
/// `target`, verifying and storing each frame when it arrives. The request carries the
/// local signed device list, except to a mirror: any node can be a gossip neighbor.
///
/// Frames stored before an error or before `deadline` stay stored; the next sync
/// continues from there. An error is returned only if no summary was received or the
/// summary could not be used.
///
/// The device list and stage announcement of the summary are used only from
/// `SyncSource::Author`: device sync exchanges device lists, and a mirror is not a device
/// of the author. Local interactions of the author that the responder lacks are deleted
/// only after a sync from `SyncSource::Author`: neither of two own devices holds the
/// complete set, and a mirror sends no interactions (interactions in a mirror's frames
/// are discarded). Posts are deleted only by signed tombstones, from any source. A
/// Public profile in the summary of `SyncSource::Author` is recorded with
/// `Storage::confirm_public_profile`.
///
/// Emits `feed-updated` once at the end if a post row was inserted or deleted or a new
/// tombstone was stored. Rows that already existed produce no event.
#[allow(clippy::too_many_arguments)]
pub(super) async fn sync_from_peer<R: Runtime>(
    endpoint: &Endpoint,
    storage: &Storage,
    identity: &SharedIdentity,
    target: EndpointId,
    author: &str,
    app_handle: &AppHandle<R>,
    label: &str,
    deadline: Instant,
    source: SyncSource,
) -> Result<SyncOneResult, AppError> {
    let start = Instant::now();
    let my_id = identity.read().await.master_pubkey.clone();
    let my_devices = if source == SyncSource::Mirror {
        None
    } else {
        Some(crate::announcement::build_own_announcement(storage, identity).await?)
    };
    let conn = tokio::time::timeout(
        SYNC_CONNECT_TIMEOUT,
        endpoint.connect(EndpointAddr::from(target), PEER_ALPN),
    )
    .await
    .map_err(|_| AppError::Other("sync connect timed out".into()))??;

    // Request: counts, newest timestamps, digest of the stored post ids.
    let post_count = storage.count_posts_by_author(author).await?;
    let interaction_count = storage.count_interactions_by_author(author).await?;
    let newest_timestamp = storage.newest_post_timestamp(author).await?;
    let newest_interaction_timestamp = storage.newest_interaction_timestamp(author).await?;
    let tombstone_count = storage.count_post_tombstones(author).await?;
    // Ordered by timestamp ascending: the last `MAX_SYNC_IDS` entries are the newest.
    let mut known_ids = storage.get_post_ids_by_author(author).await?;
    let ids_for_diff: Vec<String> = known_ids
        .iter()
        .skip(known_ids.len().saturating_sub(MAX_SYNC_IDS))
        .cloned()
        .collect();
    let ids_digest = post_ids_digest(&mut known_ids);
    drop(known_ids);

    let request = PeerRequest::Sync(SyncRequest {
        author: author.to_string(),
        post_count,
        interaction_count,
        newest_timestamp,
        newest_interaction_timestamp,
        post_ids_digest: ids_digest,
        tombstone_count,
        since: 0,
        announcement: my_devices,
    });
    let request_bytes = serde_json::to_vec(&request)?;

    let (mut send, mut recv) = tokio::time::timeout(SYNC_STEP_TIMEOUT, conn.open_bi())
        .await
        .map_err(|_| AppError::Other("sync stream open timed out".into()))??;
    write_all_timeout(&mut send, &request_bytes, SYNC_STEP_TIMEOUT).await?;
    let summary_bytes =
        read_to_end_timeout(&mut recv, SYNC_MAX_SUMMARY_BYTES, SYNC_STEP_TIMEOUT).await?;
    let summary: SyncSummary = serde_json::from_slice(&summary_bytes)?;
    let from_author = source == SyncSource::Author;
    // A mirror's post count is not the author's.
    let remote_post_count = from_author.then_some(summary.server_post_count);

    // The responder's signed device list: the current NodeIds of the author.
    if from_author && let Some(announcement) = &summary.announcement {
        if announcement.master_pubkey != author {
            log::warn!("[{label}] sync summary carries a device list of another user");
        } else if let Err(reason) = verify_linked_devices_announcement(announcement) {
            log::warn!("[{label}] invalid device announcement in sync summary: {reason}");
        } else if let Err(e) = storage
            .cache_peer_announcement(announcement, Some(&target.to_string()))
            .await
        {
            log::warn!("[{label}] rejected device announcement in sync summary: {e}");
        }
    }

    if let Some(profile) = &summary.profile {
        match verify_and_store_profile(storage, author, profile).await {
            Ok(changed) => {
                if changed {
                    let _ = app_handle.emit("profile-updated", author);
                }
                if from_author
                    && profile.visibility == proscenium_types::Visibility::Public
                    && let Err(e) = storage
                        .confirm_public_profile(author, proscenium_types::now_millis())
                        .await
                {
                    log::warn!("[{label}] failed to record the Public profile: {e}");
                }
            }
            Err(e) => log::warn!("[{label}] rejected profile in sync summary: {e}"),
        }
    }

    if from_author && let Some(stage) = &summary.active_stage {
        crate::ingest::emit_verified_stage(stage, author, app_handle, label);
    }

    log::info!(
        "[{label}] {} from {source:?} mode={:?}, remote=({}/{}/{}), local=({post_count}/{interaction_count}/{tombstone_count})",
        short_id(author),
        summary.mode,
        summary.server_post_count,
        summary.server_interaction_count,
        summary.server_tombstone_count,
    );

    if summary.mode == SyncMode::UpToDate {
        conn.close(0u32.into(), b"done");
        return Ok(SyncOneResult {
            stored: 0,
            posts: Vec::new(),
            remote_post_count,
        });
    }

    // The responder sends every interaction of the author, not only newer ones, when the
    // count difference is not explained by interactions after the newest local timestamp.
    let full_interaction_set = summary.server_interaction_count != interaction_count
        && !proscenium_types::sends_only_newer_interactions(
            summary.server_interaction_count,
            interaction_count,
            summary.interactions_after_count,
        );

    let mut ingest = FrameIngest::new(
        storage,
        author,
        label,
        &my_id,
        app_handle,
        full_interaction_set,
        source != SyncSource::Mirror,
    );
    let complete = receive_frames(
        &conn,
        &mut ingest,
        summary.mode,
        &ids_for_diff,
        deadline,
        label,
    )
    .await;
    conn.close(0u32.into(), b"done");

    // The stream ended with its end marker and carried the responder's complete
    // interaction set: local interactions of the author that the responder no longer
    // has were deleted by the author and are removed.
    if complete
        && from_author
        && let Some(received_ids) = ingest.take_interaction_ids()
        && received_ids.len() as u64 == summary.server_interaction_count
    {
        match storage
            .delete_interactions_not_in(author, &received_ids)
            .await
        {
            Ok(0) => {}
            Ok(removed) => log::info!(
                "[{label}] removed {removed} interactions that {} deleted",
                short_id(author)
            ),
            Err(e) => log::error!("[{label}] failed to remove deleted interactions: {e}"),
        }
    }

    // One event per sync, and only if the stored content changed: a new post row, a
    // deleted post row, or a tombstone that was not stored before.
    if ingest.posts_stored > 0 || ingest.posts_removed > 0 || ingest.tombstones_added > 0 {
        let _ = app_handle.emit("feed-updated", ());
    }

    log::info!(
        "[{label}] stored {} posts, removed {} from {} via {} ({source:?}) in {:.1}s (mode={:?}, complete={complete})",
        ingest.posts_stored,
        ingest.posts_removed,
        short_id(author),
        target.fmt_short(),
        start.elapsed().as_secs_f64(),
        summary.mode,
    );

    Ok(SyncOneResult {
        stored: ingest.posts_stored,
        posts: ingest.take_new_posts(),
        remote_post_count,
    })
}

/// Open the data stream, send the id list if the mode requires it, and pass every frame
/// to `ingest`. Returns `true` if the stream ended with the zero-length end marker.
async fn receive_frames<R: Runtime>(
    conn: &iroh::endpoint::Connection,
    ingest: &mut FrameIngest<'_, R>,
    mode: SyncMode,
    ids_for_diff: &[String],
    deadline: Instant,
    label: &str,
) -> bool {
    let opened = tokio::time::timeout(SYNC_STEP_TIMEOUT, conn.open_bi()).await;
    let (mut data_send, mut data_recv) = match opened {
        Ok(Ok(streams)) => streams,
        Ok(Err(e)) => {
            log::warn!("[{label}] failed to open sync data stream: {e}");
            return false;
        }
        Err(_) => {
            log::warn!("[{label}] opening the sync data stream timed out");
            return false;
        }
    };

    let id_list = if mode == SyncMode::NeedIdDiff {
        match serde_json::to_vec(ids_for_diff) {
            Ok(bytes) => bytes,
            Err(e) => {
                log::error!("[{label}] failed to encode post id list: {e}");
                return false;
            }
        }
    } else {
        Vec::new()
    };
    if let Err(e) = write_all_timeout(&mut data_send, &id_list, SYNC_STEP_TIMEOUT).await {
        log::warn!("[{label}] failed to send post id list: {e}");
        return false;
    }

    loop {
        if Instant::now() >= deadline {
            log::warn!("[{label}] sync time limit reached, keeping the frames stored so far");
            return false;
        }
        let frame_bytes = match read_frame_limited(&mut data_recv, SYNC_MAX_FRAME_BYTES).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return true,
            Err(e) => {
                log::warn!("[{label}] sync frame read failed: {e}");
                return false;
            }
        };
        let frame: SyncFrame = match serde_json::from_slice(&frame_bytes) {
            Ok(frame) => frame,
            Err(e) => {
                log::warn!("[{label}] sync frame does not parse: {e}");
                return false;
            }
        };
        drop(frame_bytes);
        if let Err(e) = ingest.process(frame).await {
            log::warn!("[{label}] sync stopped: {e}");
            return false;
        }
    }
}
