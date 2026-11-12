//! Sync between the devices of one user over the peer ALPN.
//!
//! Every device holds the master key. Each side sends its device list signed by the
//! master key and accepts the other side only if that list is signed by the local master
//! key and lists the NodeId of the connection, which QUIC authenticates, and the NodeId
//! was not removed. Both sides store the devices of the other side's list, and its
//! removed NodeIds if the other side is a stored device of the local user. A device whose
//! NodeId such a list removes stops the exchange and asks the user whether to delete its
//! data (`crate::setup::confirm_removed_device_reset`). A removed device holds the master
//! key, so a removal never deletes data without that confirmation.
//!
//! Stream 1 (opened by the requester): JSON `PeerRequest::DeviceSyncRequest` with the
//! requester's device list, then finish. The responder answers with the JSON of its own
//! device list, then finishes. The responder sends its list before it checks whether the
//! requester was removed, so a removed requester receives the list that removes it.
//!
//! Stream 2 (opened by the requester), `DeviceSyncFrame`s in the framing of
//! `crate::framing`:
//! 1. requester: `Vector`.
//! 2. responder: `Vector`, its data frames, a zero-length frame, then finish.
//! 3. requester: its data frames, a zero-length frame, then finish.
//!
//! Data frames: all follow, moderation and bookmark rows and the device lists of the
//! followed users unless both vectors carry the same `lww_digest`; the direct messages
//! among the sender's newest
//! `MAX_DEVICE_SYNC_DM_IDS`, and the deletions of own interactions among the sender's
//! newest `MAX_SYNC_TOMBSTONES`, whose ids the receiver's vector does not list.
//!
//! After the exchange, each side fetches the own posts, interactions and post deletions
//! of the other side with the sync protocol (`crate::sync::sync_from_own_device`).

use crate::announcement::{build_own_announcement, verify_lists_device};
use crate::error::AppError;
use crate::framing::{read_frame, write_frame};
use crate::gossip::GossipService;
use crate::state::{AppState, SharedIdentity};
use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, RecvStream, SendStream, VarInt},
    protocol::AcceptError,
};
use proscenium_types::{
    BookmarkEntry, DeviceSyncFrame, DeviceSyncVector, DirectMessage, FollowEntry,
    FollowRequestEntry, LinkedDevicesAnnouncement, MAX_ANNOUNCEMENT_JSON_LEN,
    MAX_DEVICE_SYNC_DM_IDS, MAX_SYNC_TOMBSTONES, ModerationEntry, PEER_ALPN, PeerRequest,
    StoredMessage, is_valid_id, is_valid_pubkey, now_millis, short_id,
    verify_linked_devices_announcement,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// Time limit for connecting and for opening or accepting a stream.
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// Time limit for reading or writing one frame.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);
/// Time limit for one device sync, in either role.
const DEVICE_SYNC_TIMEOUT: Duration = Duration::from_secs(120);
/// Data frames accepted from the other device in one sync.
const MAX_SYNC_FRAMES: usize = 1_000;
/// Rows, messages and ids accepted from the other device in one sync.
const MAX_SYNC_ITEMS: usize = 200_000;
/// Follow, moderation or bookmark rows per frame.
const LWW_BATCH_SIZE: usize = 500;
/// Direct messages per frame. One message is at most about 64 KiB of JSON.
const DM_BATCH_SIZE: usize = 100;
/// Tombstones per frame.
const TOMBSTONE_BATCH_SIZE: usize = 1_000;
/// Device lists of followed users per frame. One list is at most
/// `MAX_ANNOUNCEMENT_JSON_LEN` bytes of JSON.
const ANNOUNCEMENT_BATCH_SIZE: usize = 50;
/// QUIC close code for a rejected or failed device sync.
const CLOSE_FAILED: u32 = 1;

/// The last-writer-wins rows that device sync and device linking replicate: follows,
/// moderation entries (mutes and blocks) and bookmarks, removed rows included, and the
/// follow requests of other users (the later decision wins).
#[derive(Debug, Clone, Default)]
pub struct LwwState {
    pub follows: Vec<FollowEntry>,
    pub moderation: Vec<ModerationEntry>,
    pub bookmarks: Vec<BookmarkEntry>,
    pub follow_requests: Vec<FollowRequestEntry>,
}

impl LwwState {
    /// Hex SHA-256 over the key, state and `last_changed_at` of every row, in the order
    /// the rows are stored here. `Storage::load_lww_state` returns rows in key order,
    /// so two devices holding equal rows produce equal digests.
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        for f in &self.follows {
            hasher.update(format!(
                "f\n{}\n{}\n{}\n",
                f.pubkey, f.state, f.last_changed_at
            ));
        }
        for m in &self.moderation {
            hasher.update(format!(
                "m\n{}\n{}\n{}\n{}\n",
                m.pubkey, m.kind, m.state, m.last_changed_at
            ));
        }
        for b in &self.bookmarks {
            hasher.update(format!(
                "b\n{}\n{}\n{}\n",
                b.post_id, b.state, b.last_changed_at
            ));
        }
        for r in &self.follow_requests {
            hasher.update(format!("r\n{}\n{}\n{}\n", r.pubkey, r.status, r.decided_at));
        }
        hex::encode(hasher.finalize())
    }
}

/// Stats from a device sync operation.
#[derive(Debug, Default)]
pub struct DeviceSyncStats {
    /// Direct messages, interaction deletions and device lists that failed validation, or
    /// device lists of users the local user does not follow.
    pub rejected: u32,
    /// Users followed by a merged row.
    pub followed: Vec<String>,
    /// Users that were followed before a merged row removed the follow.
    pub unfollowed: Vec<String>,
    pub moderation_merged: u32,
    pub bookmarks_merged: u32,
    pub follow_requests_merged: u32,
    pub devices_added: u32,
    /// Rows of other devices deleted because the other side's list removed them.
    pub devices_removed: u32,
    /// Name of the stored device whose list removed the local device; the exchange
    /// stopped there.
    pub local_removed_by: Option<String>,
    /// Direct messages stored for the first time, by the other user of the conversation.
    pub dms_imported: BTreeMap<String, Vec<StoredMessage>>,
    /// Own interactions deleted by tombstones from the other device.
    pub interactions_removed: u32,
}

/// The other user of `message` if it is a valid message between `master_pubkey` and
/// another user: one of the two parties is `master_pubkey`, the other is a canonical
/// pubkey, the conversation id matches them, and the message passes
/// `validate_direct_message`.
fn own_dm_peer<'a>(message: &'a StoredMessage, master_pubkey: &str) -> Option<&'a str> {
    let peer = if message.from_pubkey == master_pubkey {
        message.to_pubkey.as_str()
    } else if message.to_pubkey == master_pubkey {
        message.from_pubkey.as_str()
    } else {
        return None;
    };
    let fields = DirectMessage {
        id: message.id.clone(),
        content: message.content.clone(),
        timestamp: message.timestamp,
        media: message.media.clone(),
        reply_to: message.reply_to.clone(),
    };
    (peer != master_pubkey
        && is_valid_pubkey(peer)
        && message.conversation_id == Storage::conversation_id(master_pubkey, peer)
        && crate::dm::validate_direct_message(&fields).is_ok())
    .then_some(peer)
}

/// Number of rows, messages or ids in a data frame.
fn frame_item_count(frame: &DeviceSyncFrame) -> usize {
    match frame {
        DeviceSyncFrame::Vector(_) => 0,
        DeviceSyncFrame::Follows(items) => items.len(),
        DeviceSyncFrame::PeerAnnouncements(items) => items.len(),
        DeviceSyncFrame::Moderation(items) => items.len(),
        DeviceSyncFrame::Bookmarks(items) => items.len(),
        DeviceSyncFrame::FollowRequests(items) => items.len(),
        DeviceSyncFrame::DirectMessages(items) => items.len(),
        DeviceSyncFrame::InteractionTombstones(items) => items.len(),
    }
}

/// Await `step` for at most `limit`. `what` names the step in the error.
async fn timed<T>(
    what: &str,
    limit: Duration,
    step: impl Future<Output = T>,
) -> Result<T, AppError> {
    tokio::time::timeout(limit, step)
        .await
        .map_err(|_| AppError::Other(format!("device sync: {what} timed out")))
}

async fn write_sync_frame(send: &mut SendStream, frame: &DeviceSyncFrame) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(frame)?;
    timed("write frame", FRAME_TIMEOUT, write_frame(send, &bytes)).await?
}

/// Write the zero-length frame that ends a frame sequence.
async fn write_end_frame(send: &mut SendStream) -> Result<(), AppError> {
    timed("write end frame", FRAME_TIMEOUT, write_frame(send, &[])).await?
}

/// Read one frame. Returns `None` for the zero-length frame that ends a sequence.
async fn read_sync_frame(recv: &mut RecvStream) -> Result<Option<DeviceSyncFrame>, AppError> {
    match timed("read frame", FRAME_TIMEOUT, read_frame(recv)).await?? {
        Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        None => Ok(None),
    }
}

/// Read the `Vector` frame that starts each direction of the exchange.
async fn read_vector(recv: &mut RecvStream) -> Result<DeviceSyncVector, AppError> {
    match read_sync_frame(recv).await? {
        Some(DeviceSyncFrame::Vector(vector)) => Ok(vector),
        _ => Err("device sync: expected a vector frame".into()),
    }
}

/// The two roles of the device sync exchange, on the local database and identity.
struct DeviceSync<'a> {
    storage: &'a Storage,
    identity: &'a SharedIdentity,
}

impl DeviceSync<'_> {
    /// Load the local rows and build the vector that describes the local state.
    async fn local_state(&self) -> Result<(String, LwwState, DeviceSyncVector), AppError> {
        let master_pubkey = self.identity.read().await.master_pubkey.clone();
        let lww = self.storage.load_lww_state(&master_pubkey).await?;
        let vector = DeviceSyncVector {
            lww_digest: lww.digest(),
            dm_ids: self
                .storage
                .get_newest_dm_ids(MAX_DEVICE_SYNC_DM_IDS)
                .await?,
            interaction_tombstone_ids: self
                .storage
                .get_interaction_tombstone_ids(&master_pubkey, MAX_SYNC_TOMBSTONES)
                .await?,
        };
        Ok((master_pubkey, lww, vector))
    }

    /// Return an error unless `announcement` is a device list of the local user that
    /// lists `remote`.
    async fn verify_device_list(
        &self,
        announcement: &LinkedDevicesAnnouncement,
        remote: &str,
    ) -> Result<(), AppError> {
        let master_pubkey = self.identity.read().await.master_pubkey.clone();
        verify_lists_device(announcement, &master_pubkey, remote)
    }

    /// Store `announcement`, a list that passed `verify_device_list` for `remote`.
    ///
    /// Returns an error, and stores nothing, if `remote` was removed, or if `remote` is
    /// neither a stored device nor a device this device linked (`take_pending_link`).
    /// Every device holds the master key, so a removed device that generates a new NodeId
    /// signs valid lists; it is not accepted unless a stored device's list names it.
    /// Removed NodeIds are stored only from a stored device; a newly linked device adds
    /// devices only. If a list removes the local device, sets `stats.local_removed_by`
    /// and stores nothing. Then its devices that are not stored yet are stored.
    async fn merge_device_list(
        &self,
        announcement: &LinkedDevicesAnnouncement,
        remote: &str,
        stats: &mut DeviceSyncStats,
    ) -> Result<(), AppError> {
        if self.storage.is_removed_device(remote).await? {
            return Err("device sync: the other device was removed from this identity".into());
        }
        let own_node_id = self.identity.read().await.transport_node_id.clone();
        match self.storage.get_other_device(remote).await? {
            Some(remover) => {
                let merged = self
                    .storage
                    .merge_removed_devices(&announcement.removed, &own_node_id)
                    .await?;
                if merged.local_removed {
                    stats.local_removed_by = Some(remover.device_name);
                    return Ok(());
                }
                stats.devices_removed = merged.devices_removed;
            }
            None => {
                if !self.storage.take_pending_link(remote, now_millis()).await? {
                    return Err(
                        "device sync: unknown device; no stored device lists it and this \
                         device did not link it"
                            .into(),
                    );
                }
            }
        }
        self.storage
            .record_own_announcement_version(announcement.version)
            .await?;
        let other_devices: Vec<_> = announcement
            .devices
            .iter()
            .filter(|d| d.node_id != own_node_id)
            .cloned()
            .collect();
        stats.devices_added = self
            .storage
            .insert_missing_linked_devices(&other_devices)
            .await?;
        Ok(())
    }

    /// Send the data frames the other device lacks according to `peer`, then the
    /// zero-length end frame.
    async fn send_state(
        &self,
        send: &mut SendStream,
        master_pubkey: &str,
        peer: &DeviceSyncVector,
        lww: &LwwState,
    ) -> Result<(), AppError> {
        // The receiver merges each row by `last_changed_at`, so all rows are sent
        // whenever the two sets differ.
        if lww.digest() != peer.lww_digest {
            for chunk in lww.follows.chunks(LWW_BATCH_SIZE) {
                write_sync_frame(send, &DeviceSyncFrame::Follows(chunk.to_vec())).await?;
            }
            let announcements = self
                .storage
                .get_followed_peer_announcements(master_pubkey)
                .await?;
            for chunk in announcements.chunks(ANNOUNCEMENT_BATCH_SIZE) {
                write_sync_frame(send, &DeviceSyncFrame::PeerAnnouncements(chunk.to_vec())).await?;
            }
            for chunk in lww.moderation.chunks(LWW_BATCH_SIZE) {
                write_sync_frame(send, &DeviceSyncFrame::Moderation(chunk.to_vec())).await?;
            }
            for chunk in lww.bookmarks.chunks(LWW_BATCH_SIZE) {
                write_sync_frame(send, &DeviceSyncFrame::Bookmarks(chunk.to_vec())).await?;
            }
            for chunk in lww.follow_requests.chunks(LWW_BATCH_SIZE) {
                write_sync_frame(send, &DeviceSyncFrame::FollowRequests(chunk.to_vec())).await?;
            }
        }

        let listed: HashSet<&str> = peer.dm_ids.iter().map(String::as_str).collect();
        let missing: Vec<String> = self
            .storage
            .get_newest_dm_ids(MAX_DEVICE_SYNC_DM_IDS)
            .await?
            .into_iter()
            .filter(|id| !listed.contains(id.as_str()))
            .collect();
        for chunk in missing.chunks(DM_BATCH_SIZE) {
            let messages = self.storage.get_dm_messages_by_ids(chunk).await?;
            write_sync_frame(send, &DeviceSyncFrame::DirectMessages(messages)).await?;
        }

        let listed: HashSet<&str> = peer
            .interaction_tombstone_ids
            .iter()
            .map(String::as_str)
            .collect();
        let tombstone_ids: Vec<String> = self
            .storage
            .get_interaction_tombstone_ids(master_pubkey, MAX_SYNC_TOMBSTONES)
            .await?
            .into_iter()
            .filter(|id| !listed.contains(id.as_str()))
            .collect();
        for chunk in tombstone_ids.chunks(TOMBSTONE_BATCH_SIZE) {
            write_sync_frame(
                send,
                &DeviceSyncFrame::InteractionTombstones(chunk.to_vec()),
            )
            .await?;
        }

        write_end_frame(send).await
    }

    /// Read data frames until the zero-length frame and store their content.
    ///
    /// A direct message is stored only if `own_dm_peer` accepts it; an interaction
    /// tombstone only if its id is valid; a device list only if it verifies and its user is
    /// followed (it is cached as received from no device, so it maps no NodeId). At most
    /// `MAX_SYNC_FRAMES` frames and `MAX_SYNC_ITEMS` items are accepted.
    async fn import_frames(
        &self,
        recv: &mut RecvStream,
        master_pubkey: &str,
        stats: &mut DeviceSyncStats,
    ) -> Result<(), AppError> {
        let mut frames = 0usize;
        let mut items = 0usize;

        while let Some(frame) = read_sync_frame(recv).await? {
            frames += 1;
            items = items.saturating_add(frame_item_count(&frame));
            if frames > MAX_SYNC_FRAMES || items > MAX_SYNC_ITEMS {
                return Err(AppError::Other(format!(
                    "device sync: other device sent more than {MAX_SYNC_FRAMES} frames or {MAX_SYNC_ITEMS} items"
                )));
            }

            match frame {
                DeviceSyncFrame::Vector(_) => {
                    return Err("device sync: unexpected vector frame".into());
                }
                DeviceSyncFrame::Follows(entries) => {
                    let mut was_followed = HashSet::new();
                    for entry in &entries {
                        if self
                            .storage
                            .is_following(master_pubkey, &entry.pubkey)
                            .await?
                        {
                            was_followed.insert(entry.pubkey.clone());
                        }
                    }
                    let changed = self
                        .storage
                        .merge_follows_lww(master_pubkey, &entries)
                        .await?;
                    for entry in changed {
                        if entry.state == "active" {
                            stats.followed.push(entry.pubkey);
                        } else if was_followed.contains(&entry.pubkey) {
                            stats.unfollowed.push(entry.pubkey);
                        }
                    }
                }
                DeviceSyncFrame::PeerAnnouncements(announcements) => {
                    for announcement in announcements {
                        if !self
                            .storage
                            .is_following(master_pubkey, &announcement.master_pubkey)
                            .await?
                            || verify_linked_devices_announcement(&announcement).is_err()
                        {
                            stats.rejected += 1;
                            continue;
                        }
                        self.storage
                            .cache_peer_announcement(&announcement, None)
                            .await?;
                    }
                }
                DeviceSyncFrame::Moderation(entries) => {
                    stats.moderation_merged += self.storage.merge_moderation_lww(&entries).await?;
                }
                DeviceSyncFrame::Bookmarks(entries) => {
                    stats.bookmarks_merged += self.storage.merge_bookmarks_lww(&entries).await?;
                }
                DeviceSyncFrame::FollowRequests(entries) => {
                    stats.follow_requests_merged += self
                        .storage
                        .merge_follow_requests(master_pubkey, &entries)
                        .await?;
                }
                DeviceSyncFrame::DirectMessages(messages) => {
                    for message in messages {
                        let Some(peer) = own_dm_peer(&message, master_pubkey) else {
                            stats.rejected += 1;
                            continue;
                        };
                        let incoming = message.from_pubkey == peer;
                        if self
                            .storage
                            .import_synced_dm(peer, &message, incoming)
                            .await?
                        {
                            let peer = peer.to_string();
                            stats.dms_imported.entry(peer).or_default().push(message);
                        }
                    }
                }
                DeviceSyncFrame::InteractionTombstones(ids) => {
                    for id in ids {
                        if !is_valid_id(&id) {
                            stats.rejected += 1;
                            continue;
                        }
                        if self
                            .storage
                            .apply_interaction_deletion(master_pubkey, &id, now_millis())
                            .await?
                        {
                            stats.interactions_removed += 1;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Responder side of the exchange on an accepted connection. `request_send` is the
    /// response side of stream 1, `announcement` the requester's device list.
    async fn respond(
        &self,
        conn: &Connection,
        mut request_send: SendStream,
        announcement: &LinkedDevicesAnnouncement,
    ) -> Result<DeviceSyncStats, AppError> {
        let remote = conn.remote_id().to_string();
        let mut stats = DeviceSyncStats::default();
        self.verify_device_list(announcement, &remote).await?;
        let own = serde_json::to_vec(&build_own_announcement(self.storage, self.identity).await?)?;
        timed(
            "write device list",
            STEP_TIMEOUT,
            request_send.write_all(&own),
        )
        .await??;
        request_send.finish()?;
        if let Err(e) = self
            .merge_device_list(announcement, &remote, &mut stats)
            .await
        {
            // The caller closes the connection on an error. A removed requester must
            // receive the list first.
            let _ = timed("flush device list", STEP_TIMEOUT, request_send.stopped()).await;
            return Err(e);
        }
        if stats.local_removed_by.is_some() {
            return Ok(stats);
        }

        let (mut send, mut recv) =
            timed("accept data stream", STEP_TIMEOUT, conn.accept_bi()).await??;
        let peer_vector = read_vector(&mut recv).await?;
        let (master_pubkey, lww, vector) = self.local_state().await?;
        write_sync_frame(&mut send, &DeviceSyncFrame::Vector(vector)).await?;
        self.send_state(&mut send, &master_pubkey, &peer_vector, &lww)
            .await?;
        send.finish()?;

        self.import_frames(&mut recv, &master_pubkey, &mut stats)
            .await?;
        Ok(stats)
    }

    /// Requester side of the exchange on an established connection.
    async fn request(&self, conn: &Connection) -> Result<DeviceSyncStats, AppError> {
        let (mut request_send, mut request_recv) =
            timed("open request stream", STEP_TIMEOUT, conn.open_bi()).await??;
        let request = serde_json::to_vec(&PeerRequest::DeviceSyncRequest {
            announcement: build_own_announcement(self.storage, self.identity).await?,
        })?;
        timed(
            "write request",
            STEP_TIMEOUT,
            request_send.write_all(&request),
        )
        .await??;
        request_send.finish()?;

        let mut stats = DeviceSyncStats::default();
        let answer = timed(
            "read device list",
            STEP_TIMEOUT,
            request_recv.read_to_end(MAX_ANNOUNCEMENT_JSON_LEN),
        )
        .await??;
        let announcement: LinkedDevicesAnnouncement = serde_json::from_slice(&answer)?;
        let remote = conn.remote_id().to_string();
        self.verify_device_list(&announcement, &remote).await?;
        self.merge_device_list(&announcement, &remote, &mut stats)
            .await?;
        if stats.local_removed_by.is_some() {
            return Ok(stats);
        }

        let (mut send, mut recv) =
            timed("open data stream", STEP_TIMEOUT, conn.open_bi()).await??;
        let (master_pubkey, _, vector) = self.local_state().await?;
        write_sync_frame(&mut send, &DeviceSyncFrame::Vector(vector)).await?;

        let peer_vector = read_vector(&mut recv).await?;
        self.import_frames(&mut recv, &master_pubkey, &mut stats)
            .await?;

        // The rows are loaded again: after the merge above they can equal the other
        // device's rows, in which case none are sent.
        let lww = self.storage.load_lww_state(&master_pubkey).await?;
        self.send_state(&mut send, &master_pubkey, &peer_vector, &lww)
            .await?;
        send.finish()?;
        // Wait until the other device acknowledged the stream data before the caller
        // closes the connection.
        let _ = timed("flush", STEP_TIMEOUT, send.stopped()).await;
        Ok(stats)
    }
}

/// Apply the results of a completed exchange that need more than the database.
///
/// A user followed by a merged row is queued for a gossip subscription; for a user that
/// was followed before a merged row removed the follow, the subscription is closed and
/// the user's posts are deleted, as `commands::social::unfollow_user` does. Emits
/// `feed-updated` if interactions or posts were removed, `dm-synced`
/// (`{ peer, messages }`) for each conversation with imported direct messages, and
/// `linked-devices-changed` and broadcasts the device announcement if devices were added
/// or removed.
async fn after_sync(
    storage: &Storage,
    identity: &SharedIdentity,
    gossip: &GossipService,
    app_handle: &AppHandle,
    stats: &DeviceSyncStats,
) {
    // The reconnect loop subscribes once the user's NodeIds are cached.
    for pubkey in &stats.followed {
        let _ = gossip.reconnect_tx.send(pubkey.clone());
    }
    let mut feed_changed = stats.interactions_removed > 0;
    for pubkey in &stats.unfollowed {
        gossip.unfollow_user(pubkey).await;
        match storage.delete_posts_by_author(pubkey).await {
            Ok(0) => {}
            Ok(_) => feed_changed = true,
            Err(e) => log::error!(
                "[device-sync] failed to delete posts of unfollowed {}: {e}",
                short_id(pubkey)
            ),
        }
    }
    if feed_changed {
        let _ = app_handle.emit("feed-updated", ());
    }
    for (peer, messages) in &stats.dms_imported {
        let _ = app_handle.emit(
            "dm-synced",
            serde_json::json!({ "peer": peer, "messages": messages }),
        );
    }
    if stats.devices_added == 0 && stats.devices_removed == 0 {
        return;
    }
    let _ = app_handle.emit("linked-devices-changed", ());
    match build_own_announcement(storage, identity).await {
        Ok(announcement) => {
            if let Err(e) = gossip.broadcast_linked_devices(&announcement).await {
                log::error!("[device-sync] failed to broadcast device announcement: {e}");
            }
        }
        Err(e) => log::error!("[device-sync] failed to build device announcement: {e}"),
    }
}

fn log_stats(role: &str, node_id: &str, stats: &DeviceSyncStats) {
    log::info!(
        "[device-sync] {role} sync with {}: dms={}, interactions removed={}, rejected={}, followed={}, unfollowed={}, moderation={}, bookmarks={}, follow requests={}, devices added={}, devices removed={}",
        short_id(node_id),
        stats.dms_imported.values().map(Vec::len).sum::<usize>(),
        stats.interactions_removed,
        stats.rejected,
        stats.followed.len(),
        stats.unfollowed.len(),
        stats.moderation_merged,
        stats.bookmarks_merged,
        stats.follow_requests_merged,
        stats.devices_added,
        stats.devices_removed,
    );
}

/// Start `crate::setup::confirm_removed_device_reset` on its own task: the reset stops
/// the network stack, which aborts the device sync tasks. `remover` names the device
/// whose list removed this device.
fn start_removed_device_reset(app_handle: &AppHandle, remover: &str) {
    log::warn!("[device-sync] device {remover} removed this device from the identity");
    match app_handle.try_state::<Arc<AppState>>() {
        Some(state) => {
            tokio::spawn(crate::setup::confirm_removed_device_reset(
                Arc::clone(state.inner()),
                remover.to_string(),
            ));
        }
        None => log::error!("[device-sync] no app state; the data of this device is kept"),
    }
}

/// Fetch the own posts, interactions, post deletions and profile of the device `target`,
/// then open or close the own feed topic for the stored visibility, which the profile of
/// `target` can have changed.
async fn fetch_own_content(
    gossip: &GossipService,
    storage: &Storage,
    identity: &SharedIdentity,
    app_handle: &AppHandle,
    target: EndpointId,
) {
    if let Err(e) =
        crate::sync::sync_from_own_device(&gossip.endpoint, storage, identity, target, app_handle)
            .await
    {
        log::warn!(
            "[device-sync] failed to fetch own posts from {}: {e}",
            target.fmt_short()
        );
    }
    if let Err(e) = gossip.apply_stored_visibility().await {
        log::error!("[device-sync] failed to apply the visibility to the own feed: {e}");
    }
}

/// Handle an incoming `DeviceSyncRequest` with the requester's device list
/// `announcement`. `request_send` is the response side of the request stream.
///
/// The request is served only if `announcement` is signed by the local master key and
/// lists the remote NodeId. The exchange is limited to `DEVICE_SYNC_TIMEOUT`. After it,
/// the own posts of the requester are fetched; the router aborts that when it shuts down.
pub async fn handle_device_sync(
    storage: &Storage,
    identity: &SharedIdentity,
    gossip: &GossipService,
    app_handle: &AppHandle,
    conn: &Connection,
    request_send: SendStream,
    announcement: &LinkedDevicesAnnouncement,
) -> Result<(), AcceptError> {
    let remote = conn.remote_id().to_string();
    let sync = DeviceSync { storage, identity };
    let result = match tokio::time::timeout(
        DEVICE_SYNC_TIMEOUT,
        sync.respond(conn, request_send, announcement),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err("device sync: exchange timed out".into()),
    };
    match result {
        Ok(DeviceSyncStats {
            local_removed_by: Some(remover),
            ..
        }) => {
            start_removed_device_reset(app_handle, &remover);
            Ok(())
        }
        Ok(stats) => {
            log_stats("served", &remote, &stats);
            after_sync(storage, identity, gossip, app_handle, &stats).await;
            // The requester closes the connection after its last frame was acknowledged.
            let _ = tokio::time::timeout(STEP_TIMEOUT, conn.closed()).await;
            fetch_own_content(gossip, storage, identity, app_handle, conn.remote_id()).await;
            Ok(())
        }
        Err(e) => {
            log::warn!(
                "[device-sync] sync requested by {} failed: {e}",
                short_id(&remote)
            );
            conn.close(VarInt::from_u32(CLOSE_FAILED), b"device sync failed");
            Err(AcceptError::from_err(std::io::Error::other(e.to_string())))
        }
    }
}

/// Connect to `target` and run the requester side of a device sync.
async fn sync_with_device(
    sync: &DeviceSync<'_>,
    endpoint: &Endpoint,
    target: EndpointId,
) -> Result<DeviceSyncStats, AppError> {
    let conn = timed(
        "connect",
        STEP_TIMEOUT,
        endpoint.connect(EndpointAddr::from(target), PEER_ALPN),
    )
    .await??;
    let result = sync.request(&conn).await;
    match &result {
        Ok(_) => conn.close(VarInt::from_u32(0), b"done"),
        Err(_) => conn.close(VarInt::from_u32(CLOSE_FAILED), b"device sync failed"),
    }
    result
}

/// Run a device sync with every other device in the `linked_devices` table, then fetch
/// the own posts of each device that completed it.
pub async fn sync_all_devices(state: &AppState) {
    let device_ids = match state.storage.get_other_device_node_ids().await {
        Ok(ids) => ids,
        Err(e) => {
            log::error!("[device-sync] failed to get device list: {e}");
            return;
        }
    };
    if device_ids.is_empty() {
        return;
    }

    let endpoint = state.endpoint();
    let gossip = state.gossip();
    let sync = DeviceSync {
        storage: &state.storage,
        identity: &state.identity,
    };

    for node_id in &device_ids {
        let target: EndpointId = match node_id.parse() {
            Ok(target) => target,
            Err(e) => {
                log::warn!("[device-sync] invalid node id {}: {e}", short_id(node_id));
                continue;
            }
        };

        log::info!("[device-sync] syncing with device {}...", short_id(node_id));
        match tokio::time::timeout(
            DEVICE_SYNC_TIMEOUT,
            sync_with_device(&sync, &endpoint, target),
        )
        .await
        {
            Ok(Ok(DeviceSyncStats {
                local_removed_by: Some(remover),
                ..
            })) => {
                start_removed_device_reset(&state.app_handle, &remover);
                return;
            }
            Ok(Ok(stats)) => {
                log_stats("requested", node_id, &stats);
                after_sync(
                    &state.storage,
                    &state.identity,
                    &gossip,
                    &state.app_handle,
                    &stats,
                )
                .await;
                fetch_own_content(
                    &gossip,
                    &state.storage,
                    &state.identity,
                    &state.app_handle,
                    target,
                )
                .await;
            }
            Ok(Err(e)) => {
                log::warn!(
                    "[device-sync] failed to sync with {}: {e}",
                    short_id(node_id)
                );
            }
            Err(_) => {
                log::warn!("[device-sync] timeout syncing with {}", short_id(node_id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Identity, test_support::loopback_endpoint};
    use iroh::SecretKey;
    use std::sync::Arc;

    fn node_id(seed: u8) -> String {
        SecretKey::from_bytes(&[seed; 32]).public().to_string()
    }

    fn shared_identity(master: [u8; 32], transport_seed: u8) -> SharedIdentity {
        Arc::new(tokio::sync::RwLock::new(Identity::new(
            SecretKey::from_bytes(&master),
            SecretKey::from_bytes(&[transport_seed; 32]),
        )))
    }

    /// Responder side of one device sync on `endpoint`: the request read of
    /// `PeerHandler::accept`, then `DeviceSync::respond`.
    async fn serve_one_sync(
        endpoint: Endpoint,
        storage: Arc<Storage>,
        identity: SharedIdentity,
    ) -> Result<DeviceSyncStats, AppError> {
        let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
        let conn = incoming.await.map_err(|e| AppError::Other(e.to_string()))?;
        let (send, mut recv) = conn.accept_bi().await?;
        let request = recv.read_to_end(MAX_ANNOUNCEMENT_JSON_LEN + 64).await?;
        let PeerRequest::DeviceSyncRequest { announcement } = serde_json::from_slice(&request)?
        else {
            return Err("unexpected request".into());
        };
        let sync = DeviceSync {
            storage: &storage,
            identity: &identity,
        };
        let result = sync.respond(&conn, send, &announcement).await;
        match &result {
            Ok(_) => {
                let _ = tokio::time::timeout(STEP_TIMEOUT, conn.closed()).await;
            }
            Err(_) => conn.close(VarInt::from_u32(CLOSE_FAILED), b"device sync failed"),
        }
        result
    }

    /// Run one device sync from `requester` to `responder` over loopback and return the
    /// stats of the requester and of the responder.
    async fn run_sync(
        responder: (u8, Arc<Storage>, SharedIdentity),
        requester: (u8, Arc<Storage>, SharedIdentity),
    ) -> (
        Result<DeviceSyncStats, AppError>,
        Result<DeviceSyncStats, AppError>,
    ) {
        let ep_a = loopback_endpoint(SecretKey::from_bytes(&[responder.0; 32]), PEER_ALPN).await;
        let ep_b = loopback_endpoint(SecretKey::from_bytes(&[requester.0; 32]), PEER_ALPN).await;
        let server = tokio::spawn(serve_one_sync(ep_a.clone(), responder.1, responder.2));

        let limit = Duration::from_secs(30);
        let conn = tokio::time::timeout(limit, ep_b.connect(ep_a.addr(), PEER_ALPN))
            .await
            .expect("connect timed out")
            .expect("connect");
        let sync_b = DeviceSync {
            storage: &requester.1,
            identity: &requester.2,
        };
        let stats_b = tokio::time::timeout(limit, sync_b.request(&conn))
            .await
            .expect("request timed out");
        conn.close(VarInt::from_u32(0), b"done");
        let stats_a = tokio::time::timeout(limit, server)
            .await
            .expect("responder timed out")
            .expect("responder panicked");
        ep_b.close().await;
        ep_a.close().await;
        (stats_b, stats_a)
    }

    #[tokio::test]
    async fn device_sync_over_loopback_converges_both_devices() {
        let master = [1u8; 32];
        let (seed_a, seed_b, seed_c) = (21u8, 22u8, 23u8);
        let id_a = shared_identity(master, seed_a);
        let id_b = shared_identity(master, seed_b);
        let me = id_a.read().await.master_pubkey.clone();
        let followed = SecretKey::from_bytes(&[2u8; 32]).public().to_string();
        let muted = SecretKey::from_bytes(&[3u8; 32]).public().to_string();
        let bookmark = |state: &str, changed: u64| BookmarkEntry {
            post_id: "cd".repeat(16),
            created_at: 1,
            state: state.to_string(),
            last_changed_at: changed,
        };
        let unliked = "ef".repeat(16);
        let dropped = SecretKey::from_bytes(&[4u8; 32]).public().to_string();
        let follow = |pubkey: &str, state: &str, changed: u64| FollowEntry {
            pubkey: pubkey.to_string(),
            followed_at: 1,
            state: state.to_string(),
            last_changed_at: changed,
        };
        let mut followed_devices = proscenium_types::LinkedDevicesAnnouncement {
            master_pubkey: followed.clone(),
            devices: vec![proscenium_types::DeviceEntry {
                node_id: node_id(40),
                device_name: "Linux".to_string(),
                added_at: 1,
            }],
            removed: vec![],
            version: 1,
            signature: String::new(),
        };
        proscenium_types::sign_linked_devices_announcement(
            &mut followed_devices,
            &SecretKey::from_bytes(&[2u8; 32]),
        );

        // Device A: a follow with the followee's device list, a follow that it removed, a
        // removed bookmark, a like that device B removed, its own row and the row of
        // device B.
        let a = Arc::new(Storage::open_in_memory().await.unwrap());
        a.merge_follows_lww(
            &me,
            &[
                follow(&followed, "active", 100),
                follow(&dropped, "removed", 300),
            ],
        )
        .await
        .unwrap();
        a.cache_peer_announcement(&followed_devices, None)
            .await
            .unwrap();
        a.merge_bookmarks_lww(&[bookmark("removed", 200)])
            .await
            .unwrap();
        a.upsert_linked_device(&node_id(seed_a), "Linux", true, 1)
            .await
            .unwrap();
        a.upsert_linked_device(&node_id(seed_b), "Android", false, 2)
            .await
            .unwrap();
        a.save_interaction(&proscenium_types::Interaction {
            id: unliked.clone(),
            author: me.clone(),
            kind: proscenium_types::InteractionKind::Like,
            target_post_id: "ab".repeat(16),
            target_author: followed.clone(),
            timestamp: 1_000,
            signature: String::new(),
        })
        .await
        .unwrap();
        // A message from `followed` that only device A received.
        let received = dm(&me, &followed, &followed, &me, "hi from followed");
        assert_eq!(
            a.receive_dm_message(&followed, &received, "hi")
                .await
                .unwrap(),
            crate::storage::ReceivedDm::Stored
        );

        // Device B: the follow that A removed, in its older active state, a mute, the same bookmark in its older active state, its own row and the
        // row of device C, which device A does not know.
        let b = Arc::new(Storage::open_in_memory().await.unwrap());
        b.merge_follows_lww(&me, &[follow(&dropped, "active", 100)])
            .await
            .unwrap();
        b.merge_moderation_lww(&[ModerationEntry {
            pubkey: muted.clone(),
            kind: "mute".to_string(),
            created_at: 1,
            state: "active".to_string(),
            last_changed_at: 150,
        }])
        .await
        .unwrap();
        b.merge_bookmarks_lww(&[bookmark("active", 100)])
            .await
            .unwrap();
        b.upsert_linked_device(&node_id(seed_b), "Android", true, 2)
            .await
            .unwrap();
        b.upsert_linked_device(&node_id(seed_a), "Linux", false, 1)
            .await
            .unwrap();
        b.upsert_linked_device(&node_id(seed_c), "Windows", false, 3)
            .await
            .unwrap();
        b.record_interaction_tombstone(&me, &unliked, 1)
            .await
            .unwrap();
        // A message to `followed` that device B sent, and the message A received.
        let sent = dm(&me, &followed, &me, &followed, "reply from device b");
        assert!(b.import_synced_dm(&followed, &sent, false).await.unwrap());
        assert!(
            b.import_synced_dm(&followed, &received, true)
                .await
                .unwrap()
        );

        let (stats_b, stats_a) = run_sync(
            (seed_a, a.clone(), id_a.clone()),
            (seed_b, b.clone(), id_b.clone()),
        )
        .await;
        let (stats_b, stats_a) = (stats_b.expect("requester"), stats_a.expect("responder"));

        // B followed the user with the device list A held, and removed the follow A had
        // removed.
        assert_eq!(stats_b.followed, vec![followed.clone()]);
        assert_eq!(
            b.get_peer_transport_node_ids(&followed).await.unwrap(),
            vec![node_id(40)]
        );
        assert_eq!(stats_b.unfollowed, vec![dropped.clone()]);
        assert!(!b.is_following(&me, &dropped).await.unwrap());
        assert!(stats_a.followed.is_empty() && stats_a.unfollowed.is_empty());

        // A applied the deletion of the like.
        assert_eq!(stats_a.interactions_removed, 1);
        assert!(a.is_interaction_tombstoned(&me, &unliked).await.unwrap());
        assert_eq!(stats_a.rejected + stats_b.rejected, 0);

        // A received B's message; B held both messages, so A sent none.
        let conversation = Storage::conversation_id(&me, &followed);
        for storage in [&a, &b] {
            let messages = storage
                .get_dm_messages(&conversation, 10, None, None)
                .await
                .unwrap();
            assert_eq!(messages.len(), 2);
        }
        assert_eq!(stats_a.dms_imported.len(), 1);
        assert_eq!(stats_a.dms_imported[&followed][0].id, sent.id);
        assert!(stats_b.dms_imported.is_empty());

        // A stored device C from B's list; B already knew every device of A's list.
        assert_eq!(stats_b.devices_added, 0);
        assert_eq!(stats_a.devices_added, 1);
        let mut on_a = a.get_other_device_node_ids().await.unwrap();
        on_a.sort();
        let mut expected = vec![node_id(seed_b), node_id(seed_c)];
        expected.sort();
        assert_eq!(on_a, expected);
        let mut on_b = b.get_other_device_node_ids().await.unwrap();
        on_b.sort();
        let mut expected = vec![node_id(seed_a), node_id(seed_c)];
        expected.sort();
        assert_eq!(on_b, expected);

        // Follow, mute and the bookmark removal are present on both devices.
        for storage in [&a, &b] {
            assert!(storage.is_following(&me, &followed).await.unwrap());
            assert!(storage.is_muted(&muted).await.unwrap());
            assert!(!storage.is_bookmarked(&"cd".repeat(16)).await.unwrap());
        }
        assert_eq!(
            a.load_lww_state(&me).await.unwrap().digest(),
            b.load_lww_state(&me).await.unwrap().digest()
        );
    }

    #[tokio::test]
    async fn device_sync_is_refused_to_a_device_of_another_identity() {
        let a = Arc::new(Storage::open_in_memory().await.unwrap());
        let b = Arc::new(Storage::open_in_memory().await.unwrap());
        let (stats_b, stats_a) = run_sync(
            (21, a.clone(), shared_identity([1u8; 32], 21)),
            (22, b, shared_identity([9u8; 32], 22)),
        )
        .await;
        assert!(stats_a.is_err());
        assert!(stats_b.is_err());
        assert!(a.get_other_device_node_ids().await.unwrap().is_empty());
    }

    /// Storage of a device `own` of an identity whose devices are `own` and `others`.
    async fn device_storage(own: u8, others: &[u8]) -> Arc<Storage> {
        let storage = Arc::new(Storage::open_in_memory().await.unwrap());
        storage
            .upsert_linked_device(&node_id(own), "Linux", true, 1)
            .await
            .unwrap();
        for &other in others {
            storage
                .upsert_linked_device(&node_id(other), "Android", false, 2)
                .await
                .unwrap();
        }
        storage
    }

    #[tokio::test]
    async fn removal_reaches_other_devices_and_the_removed_device() {
        let master = [1u8; 32];
        let (seed_a, seed_b, seed_c) = (31u8, 32u8, 33u8);
        let (id_a, id_b, id_c) = (
            shared_identity(master, seed_a),
            shared_identity(master, seed_b),
            shared_identity(master, seed_c),
        );
        let a = device_storage(seed_a, &[seed_b, seed_c]).await;
        let b = device_storage(seed_b, &[seed_a, seed_c]).await;
        let c = device_storage(seed_c, &[seed_a, seed_b]).await;
        assert!(a.remove_linked_device(&node_id(seed_c), 10).await.unwrap());

        // B learns the removal from A; A does not store C again from B's list.
        let (stats_b, stats_a) = run_sync(
            (seed_a, a.clone(), id_a.clone()),
            (seed_b, b.clone(), id_b.clone()),
        )
        .await;
        let (stats_b, stats_a) = (stats_b.expect("requester"), stats_a.expect("responder"));
        assert_eq!(stats_b.devices_removed, 1);
        assert_eq!(stats_a.devices_added, 0);
        assert!(stats_a.local_removed_by.is_none() && stats_b.local_removed_by.is_none());
        assert!(b.is_removed_device(&node_id(seed_c)).await.unwrap());
        assert_eq!(
            b.get_other_device_node_ids().await.unwrap(),
            vec![node_id(seed_a)]
        );
        assert_eq!(
            a.get_other_device_node_ids().await.unwrap(),
            vec![node_id(seed_b)]
        );

        // C requests a sync from A: A refuses it after sending the list that removes C.
        let (stats_c, stats_a) = run_sync(
            (seed_a, a.clone(), id_a.clone()),
            (seed_c, c.clone(), id_c.clone()),
        )
        .await;
        assert!(stats_a.is_err());
        assert_eq!(
            stats_c.expect("requester").local_removed_by.as_deref(),
            Some("Android")
        );

        // B requests a sync from C: C finds its removal in B's list.
        let (stats_b, stats_c) =
            run_sync((seed_c, c.clone(), id_c), (seed_b, b.clone(), id_b)).await;
        assert!(stats_b.is_err());
        assert!(stats_c.expect("responder").local_removed_by.is_some());
        // C stored nothing of the lists that removed it.
        assert!(c.get_removed_device_ids().await.unwrap().is_empty());
    }

    /// A list of the identity `master` with the devices `devices` and the removed NodeIds
    /// `removed`, signed by `master`, as a modified client can send it.
    fn crafted_list(
        master: [u8; 32],
        devices: &[u8],
        removed: &[u8],
    ) -> proscenium_types::LinkedDevicesAnnouncement {
        let key = SecretKey::from_bytes(&master);
        let mut list = proscenium_types::LinkedDevicesAnnouncement {
            master_pubkey: key.public().to_string(),
            devices: devices
                .iter()
                .map(|&seed| proscenium_types::DeviceEntry {
                    node_id: node_id(seed),
                    device_name: "Android".to_string(),
                    added_at: 1,
                })
                .collect(),
            removed: removed.iter().map(|&seed| node_id(seed)).collect(),
            version: now_millis(),
            signature: String::new(),
        };
        proscenium_types::sign_linked_devices_announcement(&mut list, &key);
        list
    }

    #[tokio::test]
    async fn removals_apply_only_from_a_stored_device() {
        let master = [1u8; 32];
        let (seed_a, seed_b, seed_c, seed_e) = (41u8, 42u8, 43u8, 44u8);
        let identity = shared_identity(master, seed_a);
        let a = device_storage(seed_a, &[seed_b, seed_c]).await;
        let sync = DeviceSync {
            storage: &a,
            identity: &identity,
        };

        // The unknown NodeId E, for example a removed device with a new transport key, is
        // refused although it holds the master key, and stores nothing.
        let from_e = crafted_list(master, &[seed_e], &[seed_a, seed_c]);
        let mut stats = DeviceSyncStats::default();
        assert!(
            sync.merge_device_list(&from_e, &node_id(seed_e), &mut stats)
                .await
                .is_err()
        );
        assert!(!a.is_other_device(&node_id(seed_e)).await.unwrap());
        assert!(a.is_other_device(&node_id(seed_c)).await.unwrap());
        assert!(a.get_removed_device_ids().await.unwrap().is_empty());

        // A device that this device linked is stored at its first sync, without its
        // removals; at its second sync it is a stored device.
        a.record_pending_link(&node_id(seed_e), now_millis())
            .await
            .unwrap();
        let mut stats = DeviceSyncStats::default();
        sync.merge_device_list(&from_e, &node_id(seed_e), &mut stats)
            .await
            .unwrap();
        assert_eq!(stats.devices_added, 1);
        assert!(stats.local_removed_by.is_none());
        assert!(a.is_other_device(&node_id(seed_c)).await.unwrap());
        assert!(a.is_other_device(&node_id(seed_e)).await.unwrap());
        // The pending record is used once.
        assert!(
            !a.take_pending_link(&node_id(seed_e), now_millis())
                .await
                .unwrap()
        );
        // From here on E is a stored device; restore the state of the rest of the test.
        a.remove_linked_device(&node_id(seed_e), 1).await.unwrap();

        // The same removal from the stored device B is reported and stores nothing.
        let from_b = crafted_list(master, &[seed_b], &[seed_a, seed_c]);
        let mut stats = DeviceSyncStats::default();
        sync.merge_device_list(&from_b, &node_id(seed_b), &mut stats)
            .await
            .unwrap();
        assert_eq!(stats.local_removed_by.as_deref(), Some("Android"));
        assert!(a.is_other_device(&node_id(seed_c)).await.unwrap());

        // B removes C: A applies it, then refuses C before reading anything of its list.
        let from_b = crafted_list(master, &[seed_b], &[seed_c]);
        let mut stats = DeviceSyncStats::default();
        sync.merge_device_list(&from_b, &node_id(seed_b), &mut stats)
            .await
            .unwrap();
        assert_eq!(stats.devices_removed, 1);
        let from_c = crafted_list(master, &[seed_c], &[seed_b]);
        let mut stats = DeviceSyncStats::default();
        assert!(
            sync.merge_device_list(&from_c, &node_id(seed_c), &mut stats)
                .await
                .is_err()
        );
        assert!(a.is_other_device(&node_id(seed_b)).await.unwrap());
    }

    /// A stored message of the conversation of `me` and `peer`, from `from` to `to`.
    fn dm(me: &str, peer: &str, from: &str, to: &str, content: &str) -> StoredMessage {
        StoredMessage {
            id: crate::util::generate_id(),
            conversation_id: Storage::conversation_id(me, peer),
            from_pubkey: from.to_string(),
            to_pubkey: to.to_string(),
            content: content.to_string(),
            timestamp: now_millis(),
            media: vec![],
            read: false,
            delivered: true,
            reply_to: None,
        }
    }

    #[test]
    fn imported_dm_must_be_between_the_local_user_and_another_user() {
        let me = SecretKey::from_bytes(&[1u8; 32]).public().to_string();
        let peer = SecretKey::from_bytes(&[2u8; 32]).public().to_string();
        let third = SecretKey::from_bytes(&[3u8; 32]).public().to_string();

        let incoming = dm(&me, &peer, &peer, &me, "in");
        assert_eq!(own_dm_peer(&incoming, &me), Some(peer.as_str()));
        let outgoing = dm(&me, &peer, &me, &peer, "out");
        assert_eq!(own_dm_peer(&outgoing, &me), Some(peer.as_str()));

        // A message between two other users.
        assert_eq!(
            own_dm_peer(&dm(&peer, &third, &peer, &third, "x"), &me),
            None
        );
        // To the local user from the local user.
        assert_eq!(own_dm_peer(&dm(&me, &me, &me, &me, "x"), &me), None);
        // A conversation id that does not belong to the two parties.
        let mut wrong_conversation = incoming.clone();
        wrong_conversation.conversation_id = Storage::conversation_id(&me, &third);
        assert_eq!(own_dm_peer(&wrong_conversation, &me), None);
        // An invalid id, and a peer that is not a canonical pubkey.
        let mut bad_id = incoming.clone();
        bad_id.id = "m1".to_string();
        assert_eq!(own_dm_peer(&bad_id, &me), None);
        let mut bad_peer = incoming;
        bad_peer.from_pubkey = "not a key".to_string();
        bad_peer.conversation_id = Storage::conversation_id(&me, "not a key");
        assert_eq!(own_dm_peer(&bad_peer, &me), None);
    }

    #[test]
    fn lww_digest_covers_state_and_change_time() {
        let follow = |state: &str, changed: u64| FollowEntry {
            pubkey: "k".to_string(),
            followed_at: 1,
            state: state.to_string(),
            last_changed_at: changed,
        };
        let digest = |entry: FollowEntry| {
            LwwState {
                follows: vec![entry],
                ..Default::default()
            }
            .digest()
        };
        assert_eq!(digest(follow("active", 5)), digest(follow("active", 5)));
        assert_ne!(digest(follow("active", 5)), digest(follow("removed", 5)));
        assert_ne!(digest(follow("active", 5)), digest(follow("active", 6)));
        assert_ne!(digest(follow("active", 5)), LwwState::default().digest());
    }
}
