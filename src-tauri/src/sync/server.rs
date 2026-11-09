use crate::constants::{
    BATCH_SIZE, PEER_CLOSE_WAIT, PEER_RESPONSE_WRITE_TIMEOUT, SYNC_FRAME_TARGET_BYTES,
    SYNC_MAX_SUMMARY_BYTES, SYNC_STEP_TIMEOUT, SYNC_TOMBSTONE_BATCH,
};
use crate::error::AppError;
use crate::framing::{read_to_end_timeout, wait_closed, write_all_timeout, write_frame};
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::{
    SecretKey,
    endpoint::{Connection, SendStream},
    protocol::AcceptError,
};
use proscenium_types::{
    MAX_SYNC_IDS, MAX_SYNC_TOMBSTONES, Post, StageAnnouncement, SyncFrame, SyncMode, SyncRequest,
    SyncSummary, Visibility, is_valid_id, post_ids_digest, short_id, sign_stage_announcement,
};

/// Byte limit of the JSON id list a requester sends in `NeedIdDiff` mode:
/// `MAX_SYNC_IDS` ids of 32 characters, each with two quotes and a comma, plus brackets.
const MAX_ID_LIST_BYTES: usize = MAX_SYNC_IDS * 35 + 2;

fn reject(reason: &'static str) -> AcceptError {
    AcceptError::from_err(std::io::Error::other(reason))
}

/// Responder side of the sync protocol, dispatched from `PeerHandler::accept`.
///
/// The `PeerRequest::Sync` request has been read; `send` is the response side of the
/// first bi-stream. Serves only the local user's own content, and only to a requester
/// that the local visibility allows.
pub async fn handle_sync(
    storage: &Storage,
    identity: &SharedIdentity,
    remote_str: &str,
    conn: &Connection,
    mut send: SendStream,
    req: SyncRequest,
    stage_handle: &crate::stage::StageActorHandle,
) -> Result<(), AcceptError> {
    let (my_pubkey, master_key) = {
        let id = identity.read().await;
        (id.master_pubkey.clone(), id.master_key.clone())
    };

    // Posts of other authors stored on this node are never served.
    if req.author != my_pubkey {
        log::warn!(
            "[sync-server] rejecting request for another author from {}",
            short_id(remote_str)
        );
        return Err(reject("author is not served by this node"));
    }

    authorize_requester(storage, &my_pubkey, remote_str).await?;

    let map_err = |e: AppError| AcceptError::from_err(e);

    // Local state for the requested author.
    let id_timestamps = storage
        .get_post_ids_with_timestamps(&my_pubkey)
        .await
        .map_err(map_err)?;
    let server_post_count = id_timestamps.len() as u64;
    let server_interaction_count = storage
        .count_interactions_by_author(&my_pubkey)
        .await
        .map_err(map_err)?;
    let server_tombstone_count = storage
        .count_post_tombstones(&my_pubkey)
        .await
        .map_err(map_err)?;
    let posts_after_count = storage
        .count_posts_after(&my_pubkey, req.newest_timestamp)
        .await
        .map_err(map_err)?;
    let interactions_after_count = storage
        .count_interactions_after(&my_pubkey, req.newest_interaction_timestamp)
        .await
        .map_err(map_err)?;

    let mode = select_mode(
        &req,
        &id_timestamps,
        server_interaction_count,
        server_tombstone_count,
    );

    let own_identity = crate::announcement::build_own_identity_response(storage, identity)
        .await
        .map_err(map_err)?;
    let active_stage = stage_handle
        .get_active_announcement()
        .await
        .and_then(|stage| sign_own_stage(stage, &my_pubkey, &master_key));

    log::info!(
        "[sync-server] requester={}, client=({}/{}/{}), server=({server_post_count}/{server_interaction_count}/{server_tombstone_count}), mode={mode:?}",
        short_id(remote_str),
        req.post_count,
        req.interaction_count,
        req.tombstone_count,
    );

    let mut summary = SyncSummary {
        server_post_count,
        server_interaction_count,
        posts_after_count,
        interactions_after_count,
        mode,
        profile: own_identity.profile,
        active_stage,
        announcement: Some(own_identity.announcement),
        server_tombstone_count,
    };
    let mut summary_bytes = serde_json::to_vec(&summary).map_err(AcceptError::from_err)?;
    // A requester reads at most `SYNC_MAX_SUMMARY_BYTES`. The stage announcement is the
    // only field without a length bound; it is omitted if the summary would not fit.
    if summary_bytes.len() > SYNC_MAX_SUMMARY_BYTES && summary.active_stage.take().is_some() {
        summary_bytes = serde_json::to_vec(&summary).map_err(AcceptError::from_err)?;
    }
    write_all_timeout(&mut send, &summary_bytes, PEER_RESPONSE_WRITE_TIMEOUT)
        .await
        .map_err(map_err)?;

    if mode == SyncMode::UpToDate {
        wait_closed(conn, PEER_CLOSE_WAIT).await;
        return Ok(());
    }

    // Second bi-stream: the requester sends its id list (NeedIdDiff) or nothing.
    let (mut data_send, mut data_recv) = tokio::time::timeout(SYNC_STEP_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_| reject("requester did not open the data stream"))??;

    let post_ids_to_send: Vec<String> = match mode {
        SyncMode::NeedIdDiff => {
            let ids_bytes =
                read_to_end_timeout(&mut data_recv, MAX_ID_LIST_BYTES, SYNC_STEP_TIMEOUT)
                    .await
                    .map_err(map_err)?;
            let known_ids: Vec<String> =
                serde_json::from_slice(&ids_bytes).map_err(AcceptError::from_err)?;
            drop(ids_bytes);
            if known_ids.len() > MAX_SYNC_IDS || !known_ids.iter().all(|id| is_valid_id(id)) {
                return Err(reject("invalid post id list"));
            }
            storage
                .get_missing_post_ids(&my_pubkey, &known_ids)
                .await
                .map_err(map_err)?
        }
        _ => id_timestamps
            .iter()
            .filter(|(_, timestamp)| *timestamp > req.newest_timestamp)
            .map(|(id, _)| id.clone())
            .collect(),
    };
    drop(id_timestamps);

    // 1. Tombstones, only if the counts differ.
    if req.tombstone_count != server_tombstone_count {
        let tombstones = storage
            .get_post_tombstones(&my_pubkey, MAX_SYNC_TOMBSTONES)
            .await
            .map_err(map_err)?;
        for batch in tombstones.chunks(SYNC_TOMBSTONE_BATCH) {
            write_sync_frame(&mut data_send, &SyncFrame::Tombstones(batch.to_vec()))
                .await
                .map_err(map_err)?;
        }
    }

    // 2. Posts, oldest first.
    let mut posts_sent = 0usize;
    for id_batch in post_ids_to_send.chunks(BATCH_SIZE) {
        let posts = storage
            .get_posts_by_ids(&my_pubkey, id_batch)
            .await
            .map_err(map_err)?;
        posts_sent += posts.len();
        write_post_frames(&mut data_send, posts)
            .await
            .map_err(map_err)?;
    }

    // 3. Interactions, only if the counts differ: those after the requester's newest
    // timestamp if that accounts for the whole difference, otherwise all.
    if req.interaction_count != server_interaction_count {
        let only_newer = super::sends_only_newer_interactions(
            server_interaction_count,
            req.interaction_count,
            interactions_after_count,
        );
        let mut offset = 0usize;
        loop {
            let batch = if only_newer {
                storage
                    .get_interactions_after(
                        &my_pubkey,
                        req.newest_interaction_timestamp,
                        BATCH_SIZE,
                        offset,
                    )
                    .await
            } else {
                storage
                    .get_interactions_paged(&my_pubkey, BATCH_SIZE, offset)
                    .await
            }
            .map_err(map_err)?;
            if batch.is_empty() {
                break;
            }
            offset += batch.len();
            write_sync_frame(&mut data_send, &SyncFrame::Interactions(batch))
                .await
                .map_err(map_err)?;
        }
    }

    // 4. End marker.
    write_frame(&mut data_send, &[]).await.map_err(map_err)?;
    data_send.finish().map_err(AcceptError::from_err)?;

    log::info!(
        "[sync-server] sent {posts_sent} posts to {} (mode={mode:?})",
        short_id(remote_str),
    );

    wait_closed(conn, PEER_CLOSE_WAIT).await;
    Ok(())
}

/// Mode rule of the sync protocol. `id_timestamps` holds `(id, timestamp)` of every post
/// the responder stores for the requested author.
///
/// `UpToDate`: the post count, the post id digest, the interaction count and the
/// tombstone count of the request equal the responder's. Otherwise `TimestampCatchUp`:
/// the request's digest equals the digest over the responder's posts with a timestamp
/// up to the requester's newest, so the posts after that timestamp complete the
/// requester's set. Otherwise `NeedIdDiff`.
fn select_mode(
    req: &SyncRequest,
    id_timestamps: &[(String, u64)],
    interaction_count: u64,
    tombstone_count: u64,
) -> SyncMode {
    let mut all_ids: Vec<String> = id_timestamps.iter().map(|(id, _)| id.clone()).collect();
    if req.post_count == all_ids.len() as u64
        && req.interaction_count == interaction_count
        && req.tombstone_count == tombstone_count
        && req.post_ids_digest == post_ids_digest(&mut all_ids)
    {
        return SyncMode::UpToDate;
    }
    let mut ids_up_to_newest: Vec<String> = id_timestamps
        .iter()
        .filter(|(_, timestamp)| *timestamp <= req.newest_timestamp)
        .map(|(id, _)| id.clone())
        .collect();
    if req.post_ids_digest == post_ids_digest(&mut ids_up_to_newest) {
        SyncMode::TimestampCatchUp
    } else {
        SyncMode::NeedIdDiff
    }
}

/// Return an error unless the local visibility allows the NodeId `remote_str` to sync.
///
/// Public: anyone. Listed: approved followers. Private: approved followers that the local
/// user follows. A failed visibility or follower lookup rejects the request.
async fn authorize_requester(
    storage: &Storage,
    my_pubkey: &str,
    remote_str: &str,
) -> Result<(), AcceptError> {
    let visibility = storage
        .get_visibility(my_pubkey)
        .await
        .map_err(AcceptError::from_err)?;
    if visibility == Visibility::Public {
        return Ok(());
    }
    let Some(remote_pubkey) = storage
        .try_get_master_pubkey_for_transport(remote_str)
        .await
        .map_err(AcceptError::from_err)?
    else {
        log::warn!(
            "[sync-server] rejecting unknown NodeId {} ({visibility} profile)",
            short_id(remote_str)
        );
        return Err(reject("not authorized"));
    };
    let allowed = match visibility {
        Visibility::Private => storage.is_mutual(my_pubkey, &remote_pubkey).await,
        _ => storage.is_approved_follower(&remote_pubkey).await,
    }
    .map_err(AcceptError::from_err)?;
    if !allowed {
        log::warn!(
            "[sync-server] rejecting {} ({visibility} profile)",
            short_id(&remote_pubkey)
        );
        return Err(reject("not authorized"));
    }
    Ok(())
}

/// Sign the active stage announcement with the local master key.
/// Returns `None` if the local user is not the host in the announcement.
fn sign_own_stage(
    mut stage: StageAnnouncement,
    my_pubkey: &str,
    master_key: &SecretKey,
) -> Option<StageAnnouncement> {
    if stage.host_pubkey != my_pubkey {
        return None;
    }
    stage.signature = sign_stage_announcement(
        &stage.stage_id,
        &stage.title,
        &stage.ticket,
        &stage.host_pubkey,
        stage.started_at,
        master_key,
    );
    Some(stage)
}

async fn write_sync_frame(send: &mut SendStream, frame: &SyncFrame) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(frame)?;
    write_frame(send, &bytes).await
}

/// Send `posts` as `Posts` frames in order. A frame whose JSON exceeds
/// `SYNC_FRAME_TARGET_BYTES` is split in two until it fits or holds one post.
async fn write_post_frames(send: &mut SendStream, posts: Vec<Post>) -> Result<(), AppError> {
    if posts.is_empty() {
        return Ok(());
    }
    // Batches still to send; the next one is at the end.
    let mut pending = vec![posts];
    while let Some(batch) = pending.pop() {
        let frame = SyncFrame::Posts(batch);
        let bytes = serde_json::to_vec(&frame)?;
        if bytes.len() > SYNC_FRAME_TARGET_BYTES
            && let SyncFrame::Posts(mut batch) = frame
            && batch.len() > 1
        {
            let second_half = batch.split_off(batch.len() / 2);
            pending.push(second_half);
            pending.push(batch);
            continue;
        }
        write_frame(send, &bytes).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(posts: &[(&str, u64)]) -> Vec<(String, u64)> {
        posts
            .iter()
            .map(|(id, timestamp)| (id.to_string(), *timestamp))
            .collect()
    }

    /// The request a requester holding `posts` builds, as `sync_from_peer` does.
    fn request(posts: &[(&str, u64)], interaction_count: u64, tombstone_count: u64) -> SyncRequest {
        let mut ids: Vec<String> = posts.iter().map(|(id, _)| id.to_string()).collect();
        SyncRequest {
            author: "author".to_string(),
            post_count: posts.len() as u64,
            interaction_count,
            newest_timestamp: posts.iter().map(|(_, ts)| *ts).max().unwrap_or(0),
            newest_interaction_timestamp: 0,
            post_ids_digest: post_ids_digest(&mut ids),
            tombstone_count,
        }
    }

    #[test]
    fn mode_is_up_to_date_only_if_counts_and_ids_are_equal() {
        let server = rows(&[("a", 10), ("b", 20)]);
        assert_eq!(
            select_mode(&request(&[("b", 20), ("a", 10)], 3, 1), &server, 3, 1),
            SyncMode::UpToDate
        );
        // Same post count, different ids: one delete and one new post were missed.
        assert_eq!(
            select_mode(&request(&[("a", 10), ("x", 15)], 3, 1), &server, 3, 1),
            SyncMode::NeedIdDiff
        );
        // Equal posts, different interaction or tombstone count: not up to date, and no
        // id list is required.
        assert_eq!(
            select_mode(&request(&[("a", 10), ("b", 20)], 2, 1), &server, 3, 1),
            SyncMode::TimestampCatchUp
        );
        assert_eq!(
            select_mode(&request(&[("a", 10), ("b", 20)], 3, 0), &server, 3, 1),
            SyncMode::TimestampCatchUp
        );
    }

    #[test]
    fn mode_is_timestamp_catch_up_for_a_complete_prefix() {
        let server = rows(&[("a", 10), ("b", 20), ("c", 30)]);
        assert_eq!(
            select_mode(&request(&[("a", 10)], 0, 0), &server, 0, 0),
            SyncMode::TimestampCatchUp
        );
        // A requester without posts receives all posts by timestamp.
        assert_eq!(
            select_mode(&request(&[], 0, 0), &server, 0, 0),
            SyncMode::TimestampCatchUp
        );
    }

    #[test]
    fn mode_is_id_diff_when_the_requester_lacks_or_keeps_an_older_post() {
        let server = rows(&[("a", 10), ("b", 20), ("c", 30)]);
        // Lacks "a", which is older than its newest post.
        assert_eq!(
            select_mode(&request(&[("b", 20)], 0, 0), &server, 0, 0),
            SyncMode::NeedIdDiff
        );
        // Holds "x", which the responder deleted, and lacks "a"; the count difference
        // alone (1) equals the number of newer posts (1).
        assert_eq!(
            select_mode(&request(&[("x", 5), ("b", 20)], 0, 0), &server, 0, 0),
            SyncMode::NeedIdDiff
        );
        // Lacks one of two posts that share its newest timestamp.
        let server = rows(&[("a", 10), ("b", 10)]);
        assert_eq!(
            select_mode(&request(&[("a", 10)], 0, 0), &server, 0, 0),
            SyncMode::NeedIdDiff
        );
    }
}
