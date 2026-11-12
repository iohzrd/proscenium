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
    LinkedDevicesAnnouncement, MAX_SYNC_IDS, MAX_SYNC_TOMBSTONES, Post, StageAnnouncement,
    SyncFrame, SyncMode, SyncRequest, SyncSummary, Visibility, is_valid_id, post_ids_digest,
    short_id, sign_stage_announcement, verify_linked_devices_announcement,
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
/// first bi-stream. Serves the local user's own content to a requester that the local
/// visibility allows. As a mirror, serves the stored posts, post deletions and profile of
/// another author under the conditions of `authorize_mirror_requester`; a mirror sends
/// no interactions (its summary repeats the requester's interaction count), no device
/// list and no stage announcement.
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

    let author = req.author.as_str();
    let mirror = author != my_pubkey;
    if mirror {
        authorize_mirror_requester(
            storage,
            &my_pubkey,
            author,
            remote_str,
            req.announcement.as_ref(),
        )
        .await?;
    } else {
        authorize_requester(storage, &my_pubkey, remote_str, req.announcement.as_ref()).await?;
    }

    let map_err = |e: AppError| AcceptError::from_err(e);

    // Local state for the requested author: posts and interactions with a timestamp at or
    // after `req.since`, which is `timestamp > req.since - 1` (all for 0).
    let lower = req.since.checked_sub(1);
    let after = |newest: u64| lower.map_or(newest, |lower| newest.max(lower));
    let mut id_timestamps = storage
        .get_post_ids_with_timestamps(author)
        .await
        .map_err(map_err)?;
    id_timestamps.retain(|(_, timestamp)| *timestamp >= req.since);
    let server_post_count = id_timestamps.len() as u64;
    let (server_interaction_count, interactions_after_count) = if mirror {
        (req.interaction_count, 0)
    } else {
        let count = match lower {
            None => storage.count_interactions_by_author(author).await,
            Some(lower) => storage.count_interactions_after(author, lower).await,
        }
        .map_err(map_err)?;
        let after_count = storage
            .count_interactions_after(author, after(req.newest_interaction_timestamp))
            .await
            .map_err(map_err)?;
        (count, after_count)
    };
    let server_tombstone_count = storage
        .count_post_tombstones(author)
        .await
        .map_err(map_err)?;
    let posts_after_count = storage
        .count_posts_after(author, after(req.newest_timestamp))
        .await
        .map_err(map_err)?;

    let mode = select_mode(
        &req,
        &id_timestamps,
        server_interaction_count,
        server_tombstone_count,
    );

    let (profile, announcement, active_stage) = if mirror {
        let profile = storage.get_profile(author).await.map_err(map_err)?;
        (profile, None, None)
    } else {
        let own_identity = crate::announcement::build_own_identity_response(storage, identity)
            .await
            .map_err(map_err)?;
        let active_stage = stage_handle
            .get_active_announcement()
            .await
            .and_then(|stage| sign_own_stage(stage, &my_pubkey, &master_key));
        (
            own_identity.profile,
            Some(own_identity.announcement),
            active_stage,
        )
    };

    log::info!(
        "[sync-server] requester={}, author={}{}, client=({}/{}/{}), server=({server_post_count}/{server_interaction_count}/{server_tombstone_count}), mode={mode:?}",
        short_id(remote_str),
        short_id(author),
        if mirror { " (mirror)" } else { "" },
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
        profile,
        active_stage,
        announcement,
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
            let wanted: std::collections::HashSet<&str> =
                id_timestamps.iter().map(|(id, _)| id.as_str()).collect();
            let mut missing = storage
                .get_missing_post_ids(author, &known_ids)
                .await
                .map_err(map_err)?;
            missing.retain(|id| wanted.contains(id.as_str()));
            missing
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
            .get_post_tombstones(author, MAX_SYNC_TOMBSTONES)
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
            .get_posts_by_ids(author, id_batch)
            .await
            .map_err(map_err)?;
        posts_sent += posts.len();
        write_post_frames(&mut data_send, posts)
            .await
            .map_err(map_err)?;
    }

    // 3. Interactions, only if the counts differ: those after the requester's newest
    // timestamp if that accounts for the whole difference, otherwise all. A mirror
    // reports the requester's count, so it sends none.
    if req.interaction_count != server_interaction_count {
        let only_newer = proscenium_types::sends_only_newer_interactions(
            server_interaction_count,
            req.interaction_count,
            interactions_after_count,
        );
        let mut offset = 0usize;
        loop {
            let batch = match (only_newer, lower) {
                (false, None) => {
                    storage
                        .get_interactions_paged(author, BATCH_SIZE, offset)
                        .await
                }
                (false, Some(lower)) => {
                    storage
                        .get_interactions_after(author, lower, BATCH_SIZE, offset)
                        .await
                }
                (true, _) => {
                    storage
                        .get_interactions_after(
                            author,
                            after(req.newest_interaction_timestamp),
                            BATCH_SIZE,
                            offset,
                        )
                        .await
                }
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
        "[sync-server] sent {posts_sent} posts of {} to {} (mode={mode:?})",
        short_id(author),
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

/// Return an error unless the NodeId `remote_str` is another device of the local user or
/// the local visibility allows it to sync.
///
/// The requester is identified by `identify_unblocked_requester`, which rejects a blocked
/// requester at any visibility. Then Public: anyone. Listed: approved followers.
/// Private: approved followers that the local user follows.
/// The list is cached only for an allowed requester, so unknown requesters do not add
/// rows. A failed device, block, visibility or follower lookup rejects the request.
async fn authorize_requester(
    storage: &Storage,
    my_pubkey: &str,
    remote_str: &str,
    announcement: Option<&LinkedDevicesAnnouncement>,
) -> Result<(), AcceptError> {
    if storage
        .is_other_device(remote_str)
        .await
        .map_err(AcceptError::from_err)?
    {
        return Ok(());
    }
    let (remote_pubkey, presented) =
        identify_unblocked_requester(storage, my_pubkey, remote_str, announcement).await?;
    let visibility = storage
        .get_visibility(my_pubkey)
        .await
        .map_err(AcceptError::from_err)?;
    if visibility == Visibility::Public {
        return Ok(());
    }
    let Some(remote_pubkey) = remote_pubkey else {
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
    if let Some(a) = presented
        && let Err(e) = storage.cache_peer_announcement(a, Some(remote_str)).await
    {
        log::warn!(
            "[sync-server] failed to store the device list of {}: {e}",
            short_id(&remote_pubkey)
        );
    }
    Ok(())
}

/// The master pubkey of the requesting NodeId `remote_str`, if known, and the device
/// list that identified it, if any. Returns an error if that master is blocked.
///
/// The requester is the master of `announcement` if that list verifies, is not the local
/// user's and lists `remote_str`, which QUIC authenticates; otherwise the master that
/// `remote_str` is mapped to, if any.
async fn identify_unblocked_requester<'a>(
    storage: &Storage,
    my_pubkey: &str,
    remote_str: &str,
    announcement: Option<&'a LinkedDevicesAnnouncement>,
) -> Result<(Option<String>, Option<&'a LinkedDevicesAnnouncement>), AcceptError> {
    let presented = announcement.filter(|a| {
        a.master_pubkey != my_pubkey
            && a.devices.iter().any(|d| d.node_id == remote_str)
            && verify_linked_devices_announcement(a).is_ok()
    });
    let remote_pubkey = match presented {
        Some(a) => Some(a.master_pubkey.clone()),
        None => storage
            .try_get_master_pubkey_for_transport(remote_str)
            .await
            .map_err(AcceptError::from_err)?,
    };
    if let Some(pubkey) = &remote_pubkey
        && storage
            .is_blocked(pubkey)
            .await
            .map_err(AcceptError::from_err)?
    {
        log::warn!("[sync-server] rejecting blocked user {}", short_id(pubkey));
        return Err(reject("not authorized"));
    }
    Ok((remote_pubkey, presented))
}

/// Return an error unless the local node serves, as a mirror, the content of `author`,
/// another user, to the NodeId `remote_str`: the local user follows `author`, has not
/// muted or blocked `author`, the stored profile of `author` is Public, and the requester
/// is not blocked (see `identify_unblocked_requester`).
///
/// The author signs its posts, deletions and profile, so a requester verifies a mirror's
/// response as it verifies the author's. Only followed authors are served: a request for
/// another stored author would reveal which profiles the local user viewed. Every push of
/// a non-Public author carries its profile (`GossipService::attempt_push`), so a mirror
/// that holds non-Public posts also holds the non-Public profile and refuses.
async fn authorize_mirror_requester(
    storage: &Storage,
    my_pubkey: &str,
    author: &str,
    remote_str: &str,
    announcement: Option<&LinkedDevicesAnnouncement>,
) -> Result<(), AcceptError> {
    let map_err = AcceptError::from_err;
    let public = storage
        .get_profile(author)
        .await
        .map_err(map_err)?
        .is_some_and(|p| p.visibility == Visibility::Public);
    if !public
        || !storage
            .is_following(my_pubkey, author)
            .await
            .map_err(map_err)?
        || storage.is_hidden(author).await.map_err(map_err)?
    {
        log::debug!(
            "[sync-server] not a mirror of {} for {}",
            short_id(author),
            short_id(remote_str)
        );
        return Err(reject("author is not served by this node"));
    }
    identify_unblocked_requester(storage, my_pubkey, remote_str, announcement).await?;
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
    use proscenium_types::Profile;

    #[tokio::test]
    async fn other_devices_may_sync_a_private_profile() {
        let storage = Storage::open_in_memory().await.unwrap();
        let node_id = |seed: u8| SecretKey::from_bytes(&[seed; 32]).public().to_string();
        let me = node_id(1);
        let profile = Profile {
            display_name: "me".to_string(),
            bio: String::new(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility: Visibility::Private,
            updated_at: 1,
            signature: String::new(),
        };
        storage.save_profile(&me, &profile).await.unwrap();
        storage
            .upsert_linked_device(&node_id(2), "Linux", true, 1)
            .await
            .unwrap();
        storage
            .upsert_linked_device(&node_id(3), "Android", false, 2)
            .await
            .unwrap();

        assert!(
            authorize_requester(&storage, &me, &node_id(3), None)
                .await
                .is_ok()
        );
        // The local device row and an unknown NodeId are not other devices.
        for remote in [node_id(2), node_id(4)] {
            assert!(
                authorize_requester(&storage, &me, &remote, None)
                    .await
                    .is_err()
            );
        }
    }

    /// The device list `node_ids` of the identity with master seed `master`, signed.
    fn device_list(master: u8, node_ids: &[String]) -> LinkedDevicesAnnouncement {
        let key = SecretKey::from_bytes(&[master; 32]);
        let mut announcement = LinkedDevicesAnnouncement {
            master_pubkey: key.public().to_string(),
            devices: node_ids
                .iter()
                .map(|node_id| proscenium_types::DeviceEntry {
                    node_id: node_id.clone(),
                    device_name: "Linux".to_string(),
                    added_at: 1,
                })
                .collect(),
            removed: vec![],
            version: 1,
            signature: String::new(),
        };
        proscenium_types::sign_linked_devices_announcement(&mut announcement, &key);
        announcement
    }

    #[tokio::test]
    async fn requester_of_a_private_profile_is_identified_by_its_presented_list() {
        let storage = Storage::open_in_memory().await.unwrap();
        let node_id = |seed: u8| SecretKey::from_bytes(&[seed; 32]).public().to_string();
        let me = SecretKey::from_bytes(&[1u8; 32]).public().to_string();
        let mutual = SecretKey::from_bytes(&[20u8; 32]).public().to_string();
        let profile = Profile {
            display_name: "me".to_string(),
            bio: String::new(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility: Visibility::Private,
            updated_at: 1,
            signature: String::new(),
        };
        storage.save_profile(&me, &profile).await.unwrap();
        storage
            .merge_follows_lww(
                &me,
                &[proscenium_types::FollowEntry {
                    pubkey: mutual.clone(),
                    followed_at: 1,
                    state: "active".to_string(),
                    last_changed_at: 1,
                }],
            )
            .await
            .unwrap();
        storage
            .approve_follower_directly(&me, &mutual, 1, 1, u64::MAX)
            .await
            .unwrap();
        assert!(storage.is_mutual(&me, &mutual).await.unwrap());

        // A device of the mutual that presents the mutual's list is allowed and mapped.
        let list = device_list(20, &[node_id(21), node_id(22)]);
        assert!(
            authorize_requester(&storage, &me, &node_id(22), Some(&list))
                .await
                .is_ok()
        );
        assert_eq!(
            storage
                .get_master_pubkey_for_transport(&node_id(22))
                .await
                .as_deref(),
            Some(mutual.as_str())
        );

        // A list that does not name the requesting NodeId, and a stranger's list.
        assert!(
            authorize_requester(&storage, &me, &node_id(23), Some(&list))
                .await
                .is_err()
        );
        let stranger = device_list(30, &[node_id(31)]);
        assert!(
            authorize_requester(&storage, &me, &node_id(31), Some(&stranger))
                .await
                .is_err()
        );
        assert!(
            storage
                .get_peer_transport_node_ids(&stranger.master_pubkey)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn blocked_follower_is_refused_from_any_node_id() {
        let storage = Storage::open_in_memory().await.unwrap();
        let node_id = |seed: u8| SecretKey::from_bytes(&[seed; 32]).public().to_string();
        let me = SecretKey::from_bytes(&[1u8; 32]).public().to_string();
        let follower = SecretKey::from_bytes(&[20u8; 32]).public().to_string();
        let profile = Profile {
            display_name: "me".to_string(),
            bio: String::new(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility: Visibility::Listed,
            updated_at: 1,
            signature: String::new(),
        };
        storage.save_profile(&me, &profile).await.unwrap();
        storage
            .approve_follower_directly(&me, &follower, 1, 1, u64::MAX)
            .await
            .unwrap();
        let list = device_list(20, &[node_id(21)]);
        assert!(
            authorize_requester(&storage, &me, &node_id(21), Some(&list))
                .await
                .is_ok()
        );

        // Blocked: refused from the mapped NodeId and from a new NodeId with a fresh list,
        // also while the approval is still stored.
        storage.block_user(&follower).await.unwrap();
        assert!(storage.is_approved_follower(&follower).await.unwrap());
        let fresh = device_list(20, &[node_id(22)]);
        assert!(
            authorize_requester(&storage, &me, &node_id(21), None)
                .await
                .is_err()
        );
        assert!(
            authorize_requester(&storage, &me, &node_id(22), Some(&fresh))
                .await
                .is_err()
        );

        // Revoking the approval keeps the follower refused after an unblock.
        assert!(
            storage
                .revoke_follower(&me, &follower, 2, u64::MAX)
                .await
                .unwrap()
        );
        storage.unblock_user(&follower).await.unwrap();
        assert!(!storage.is_approved_follower(&follower).await.unwrap());
        assert!(
            authorize_requester(&storage, &me, &node_id(22), Some(&fresh))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn mirror_serves_only_a_followed_unhidden_public_author_to_unblocked_requesters() {
        let storage = Storage::open_in_memory().await.unwrap();
        let node_id = |seed: u8| SecretKey::from_bytes(&[seed; 32]).public().to_string();
        let me = SecretKey::from_bytes(&[1u8; 32]).public().to_string();
        let author_key = SecretKey::from_bytes(&[10u8; 32]);
        let author = author_key.public().to_string();
        let requester = device_list(20, &[node_id(21)]);
        let allowed = async |storage: &Storage| {
            authorize_mirror_requester(storage, &me, &author, &node_id(21), Some(&requester))
                .await
                .is_ok()
        };
        let profile = |visibility: Visibility, updated_at: u64| {
            let mut profile = Profile {
                display_name: "author".to_string(),
                bio: String::new(),
                avatar_hash: None,
                avatar_ticket: None,
                visibility,
                updated_at,
                signature: String::new(),
            };
            proscenium_types::sign_profile(&mut profile, &author, &author_key);
            profile
        };
        let follow = |state: &str, at: u64| proscenium_types::FollowEntry {
            pubkey: author.clone(),
            followed_at: 1,
            state: state.to_string(),
            last_changed_at: at,
        };

        // No stored profile, then Public but not followed.
        assert!(!allowed(&storage).await);
        storage
            .save_profile(&author, &profile(Visibility::Public, 1))
            .await
            .unwrap();
        assert!(!allowed(&storage).await);

        // Followed and Public: served.
        storage
            .merge_follows_lww(&me, &[follow("active", 1)])
            .await
            .unwrap();
        assert!(allowed(&storage).await);

        // Muted author, then blocked requester.
        storage.mute_user(&author).await.unwrap();
        assert!(!allowed(&storage).await);
        storage.unmute_user(&author).await.unwrap();
        assert!(allowed(&storage).await);
        storage.block_user(&requester.master_pubkey).await.unwrap();
        assert!(!allowed(&storage).await);
        storage
            .unblock_user(&requester.master_pubkey)
            .await
            .unwrap();
        assert!(allowed(&storage).await);

        // Listed author.
        storage
            .save_profile(&author, &profile(Visibility::Listed, 2))
            .await
            .unwrap();
        assert!(!allowed(&storage).await);
    }

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
            since: 0,
            announcement: None,
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
