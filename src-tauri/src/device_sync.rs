//! Sync between the devices of one user over the peer ALPN.
//!
//! Both devices hold the master key and derive their transport keys from it. Each side
//! checks that the other side's NodeId is such a derived key: the requester before it
//! dials, the responder on the accepted connection. QUIC authenticates the NodeId, so no
//! further proof is exchanged.
//!
//! Stream 1 (opened by the requester): JSON `PeerRequest::DeviceSyncRequest`, then finish.
//!
//! Stream 2 (opened by the requester), `DeviceSyncFrame`s in the framing of
//! `crate::framing`:
//! 1. requester: `Vector`.
//! 2. responder: `Vector`, its data frames, a zero-length frame, then finish.
//! 3. requester: its data frames, a zero-length frame, then finish.
//!
//! Data frames: own posts and interactions newer than the newest timestamp in the
//! receiver's vector; all follow, moderation and bookmark rows unless both vectors carry
//! the same `lww_digest`; the direct messages among the sender's newest
//! `MAX_DEVICE_SYNC_DM_IDS`, and the deletions of own posts and interactions among the
//! sender's newest `MAX_SYNC_TOMBSTONES`, whose ids the receiver's vector does not list.

use crate::constants::BATCH_SIZE;
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
    BookmarkEntry, DeviceSyncFrame, DeviceSyncVector, DirectMessage, FollowEntry, Interaction,
    MAX_DEVICE_SYNC_DM_IDS, MAX_LINKED_DEVICES, MAX_SYNC_TOMBSTONES, ModerationEntry, PEER_ALPN,
    PeerRequest, Post, StoredMessage, is_valid_id, is_valid_pubkey, now_millis, short_id,
    validate_interaction, validate_post, verify_delete_post_signature,
    verify_interaction_signature, verify_post_signature,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Time limit for connecting and for opening or accepting a stream.
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// Time limit for reading or writing one frame.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);
/// Time limit for one device sync, in either role.
const DEVICE_SYNC_TIMEOUT: Duration = Duration::from_secs(120);
/// Data frames accepted from the other device in one sync.
const MAX_SYNC_FRAMES: usize = 1_000;
/// Posts, interactions and rows accepted from the other device in one sync.
const MAX_SYNC_ITEMS: usize = 200_000;
/// Follow, moderation or bookmark rows per frame.
const LWW_BATCH_SIZE: usize = 500;
/// Direct messages per frame. One message is at most about 64 KiB of JSON.
const DM_BATCH_SIZE: usize = 100;
/// Tombstones per frame.
const TOMBSTONE_BATCH_SIZE: usize = 1_000;
/// QUIC close code for a rejected or failed device sync.
const CLOSE_FAILED: u32 = 1;

/// The last-writer-wins rows that device sync and device linking replicate: follows,
/// moderation entries (mutes and blocks) and bookmarks, removed rows included.
#[derive(Debug, Clone, Default)]
pub struct LwwState {
    pub follows: Vec<FollowEntry>,
    pub moderation: Vec<ModerationEntry>,
    pub bookmarks: Vec<BookmarkEntry>,
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
        hex::encode(hasher.finalize())
    }
}

/// Stats from a device sync operation.
#[derive(Debug, Default)]
pub struct DeviceSyncStats {
    pub posts_imported: u32,
    pub interactions_imported: u32,
    /// Posts and interactions that failed validation or signature verification.
    pub rejected: u32,
    pub follows_merged: u32,
    pub moderation_merged: u32,
    pub bookmarks_merged: u32,
    pub devices_added: u32,
    /// Direct messages stored for the first time, by the other user of the conversation.
    pub dms_imported: BTreeMap<String, Vec<StoredMessage>>,
    /// Own posts deleted by tombstones from the other device.
    pub posts_removed: u64,
    /// Own interactions deleted by tombstones from the other device.
    pub interactions_removed: u32,
}

/// True if `post` is authored by `master_pubkey`, passes `validate_post`, and is signed
/// by its author.
fn is_own_valid_post(post: &Post, master_pubkey: &str) -> bool {
    post.author == master_pubkey
        && validate_post(post).is_ok()
        && verify_post_signature(post).is_ok()
}

/// True if `interaction` is authored by `master_pubkey`, passes `validate_interaction`,
/// and is signed by its author.
fn is_own_valid_interaction(interaction: &Interaction, master_pubkey: &str) -> bool {
    interaction.author == master_pubkey
        && validate_interaction(interaction).is_ok()
        && verify_interaction_signature(interaction).is_ok()
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

/// Number of posts, interactions or rows in a data frame.
fn frame_item_count(frame: &DeviceSyncFrame) -> usize {
    match frame {
        DeviceSyncFrame::Vector(_) => 0,
        DeviceSyncFrame::Posts(items) => items.len(),
        DeviceSyncFrame::Interactions(items) => items.len(),
        DeviceSyncFrame::Follows(items) => items.len(),
        DeviceSyncFrame::Moderation(items) => items.len(),
        DeviceSyncFrame::Bookmarks(items) => items.len(),
        DeviceSyncFrame::DirectMessages(items) => items.len(),
        DeviceSyncFrame::PostTombstones(items) => items.len(),
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
        Some(DeviceSyncFrame::Vector(vector)) => {
            if vector.devices.len() > MAX_LINKED_DEVICES {
                return Err(AppError::Other(format!(
                    "device sync: vector lists {} devices (max {MAX_LINKED_DEVICES})",
                    vector.devices.len()
                )));
            }
            Ok(vector)
        }
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
        let vector = self
            .storage
            .build_device_sync_vector(&master_pubkey, &lww)
            .await?;
        Ok((master_pubkey, lww, vector))
    }

    /// Store the device entries of the other device's vector that are not stored yet and
    /// whose NodeId derives from the local master key.
    async fn apply_peer_vector(
        &self,
        peer: &DeviceSyncVector,
        stats: &mut DeviceSyncStats,
    ) -> Result<(), AppError> {
        let other_devices: Vec<_> = {
            let id = self.identity.read().await;
            id.own_devices(&peer.devices)
                .into_iter()
                .filter(|d| d.node_id != id.transport_node_id)
                .collect()
        };
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
        let mut offset = 0;
        loop {
            let batch = self
                .storage
                .get_posts_after(master_pubkey, peer.newest_post_ts, BATCH_SIZE, offset)
                .await?;
            if batch.is_empty() {
                break;
            }
            offset += batch.len();
            write_sync_frame(send, &DeviceSyncFrame::Posts(batch)).await?;
        }

        let mut offset = 0;
        loop {
            let batch = self
                .storage
                .get_interactions_after(
                    master_pubkey,
                    peer.newest_interaction_ts,
                    BATCH_SIZE,
                    offset,
                )
                .await?;
            if batch.is_empty() {
                break;
            }
            offset += batch.len();
            write_sync_frame(send, &DeviceSyncFrame::Interactions(batch)).await?;
        }

        // The receiver merges each row by `last_changed_at`, so all rows are sent
        // whenever the two sets differ.
        if lww.digest() != peer.lww_digest {
            for chunk in lww.follows.chunks(LWW_BATCH_SIZE) {
                write_sync_frame(send, &DeviceSyncFrame::Follows(chunk.to_vec())).await?;
            }
            for chunk in lww.moderation.chunks(LWW_BATCH_SIZE) {
                write_sync_frame(send, &DeviceSyncFrame::Moderation(chunk.to_vec())).await?;
            }
            for chunk in lww.bookmarks.chunks(LWW_BATCH_SIZE) {
                write_sync_frame(send, &DeviceSyncFrame::Bookmarks(chunk.to_vec())).await?;
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

        let listed: HashSet<&str> = peer.post_tombstone_ids.iter().map(String::as_str).collect();
        let tombstones: Vec<_> = self
            .storage
            .get_post_tombstones(master_pubkey, MAX_SYNC_TOMBSTONES)
            .await?
            .into_iter()
            .filter(|tombstone| !listed.contains(tombstone.id.as_str()))
            .collect();
        for chunk in tombstones.chunks(TOMBSTONE_BATCH_SIZE) {
            write_sync_frame(send, &DeviceSyncFrame::PostTombstones(chunk.to_vec())).await?;
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
    /// A post or interaction is stored only if it is authored by the local user, passes
    /// validation and is signed by the master key; a direct message only if
    /// `own_dm_peer` accepts it. A post tombstone is applied only if it names the local
    /// user and its signature verifies; an interaction tombstone only if its id is valid.
    /// At most `MAX_SYNC_FRAMES` frames and `MAX_SYNC_ITEMS` items are accepted.
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
                DeviceSyncFrame::Posts(posts) => {
                    for post in &posts {
                        // A post deleted on this device is not stored again from another.
                        if !is_own_valid_post(post, master_pubkey)
                            || self
                                .storage
                                .is_post_tombstoned(&post.author, &post.id)
                                .await?
                        {
                            stats.rejected += 1;
                        } else if self.storage.insert_post(post).await? {
                            stats.posts_imported += 1;
                        }
                    }
                }
                DeviceSyncFrame::Interactions(interactions) => {
                    for interaction in &interactions {
                        if !is_own_valid_interaction(interaction, master_pubkey)
                            || self
                                .storage
                                .is_interaction_tombstoned(&interaction.author, &interaction.id)
                                .await?
                        {
                            stats.rejected += 1;
                        } else if self.storage.save_interaction(interaction).await? {
                            stats.interactions_imported += 1;
                        }
                    }
                }
                DeviceSyncFrame::Follows(entries) => {
                    stats.follows_merged += self
                        .storage
                        .merge_follows_lww(master_pubkey, &entries)
                        .await?;
                }
                DeviceSyncFrame::Moderation(entries) => {
                    stats.moderation_merged += self.storage.merge_moderation_lww(&entries).await?;
                }
                DeviceSyncFrame::Bookmarks(entries) => {
                    stats.bookmarks_merged += self.storage.merge_bookmarks_lww(&entries).await?;
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
                DeviceSyncFrame::PostTombstones(tombstones) => {
                    let received = tombstones.len();
                    let valid: Vec<_> = tombstones
                        .into_iter()
                        .filter(|t| {
                            t.author == master_pubkey
                                && is_valid_id(&t.id)
                                && verify_delete_post_signature(&t.id, &t.author, &t.signature)
                                    .is_ok()
                        })
                        .collect();
                    stats.rejected += (received - valid.len()) as u32;
                    let outcome = self
                        .storage
                        .apply_post_tombstones(master_pubkey, &valid, now_millis())
                        .await?;
                    stats.posts_removed += outcome.posts_removed;
                }
                DeviceSyncFrame::InteractionTombstones(ids) => {
                    for id in ids {
                        if !is_valid_id(&id) {
                            stats.rejected += 1;
                            continue;
                        }
                        self.storage
                            .record_interaction_tombstone(master_pubkey, &id, now_millis())
                            .await?;
                        if self.storage.delete_interaction(&id, master_pubkey).await? {
                            stats.interactions_removed += 1;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Responder side of the exchange on an accepted connection.
    async fn respond(&self, conn: &Connection) -> Result<DeviceSyncStats, AppError> {
        let (mut send, mut recv) =
            timed("accept data stream", STEP_TIMEOUT, conn.accept_bi()).await??;
        let mut stats = DeviceSyncStats::default();

        let peer_vector = read_vector(&mut recv).await?;
        self.apply_peer_vector(&peer_vector, &mut stats).await?;

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
        let (mut request_send, _request_recv) =
            timed("open request stream", STEP_TIMEOUT, conn.open_bi()).await??;
        let request = serde_json::to_vec(&PeerRequest::DeviceSyncRequest)?;
        timed(
            "write request",
            STEP_TIMEOUT,
            request_send.write_all(&request),
        )
        .await??;
        request_send.finish()?;

        let (mut send, mut recv) =
            timed("open data stream", STEP_TIMEOUT, conn.open_bi()).await??;
        let mut stats = DeviceSyncStats::default();

        let (master_pubkey, _, vector) = self.local_state().await?;
        write_sync_frame(&mut send, &DeviceSyncFrame::Vector(vector)).await?;

        let peer_vector = read_vector(&mut recv).await?;
        self.apply_peer_vector(&peer_vector, &mut stats).await?;
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
/// Emits `feed-updated` if posts were imported or removed, `dm-synced`
/// (`{ peer, messages }`) for each conversation with imported direct messages, and
/// broadcasts the device announcement if the device list changed.
async fn after_sync(
    storage: &Storage,
    identity: &SharedIdentity,
    gossip: &GossipService,
    app_handle: &AppHandle,
    stats: &DeviceSyncStats,
) {
    if stats.posts_imported > 0 || stats.posts_removed > 0 || stats.interactions_removed > 0 {
        let _ = app_handle.emit("feed-updated", ());
    }
    for (peer, messages) in &stats.dms_imported {
        let _ = app_handle.emit(
            "dm-synced",
            serde_json::json!({ "peer": peer, "messages": messages }),
        );
    }
    if stats.devices_added == 0 {
        return;
    }
    match crate::announcement::build_own_identity_response(storage, identity).await {
        Ok(response) => {
            if let Err(e) = gossip
                .broadcast_linked_devices(&response.announcement)
                .await
            {
                log::error!("[device-sync] failed to broadcast device announcement: {e}");
            }
        }
        Err(e) => log::error!("[device-sync] failed to build device announcement: {e}"),
    }
}

fn log_stats(role: &str, node_id: &str, stats: &DeviceSyncStats) {
    log::info!(
        "[device-sync] {role} sync with {}: posts={}, interactions={}, dms={}, posts removed={}, interactions removed={}, rejected={}, follows={}, moderation={}, bookmarks={}, devices={}",
        short_id(node_id),
        stats.posts_imported,
        stats.interactions_imported,
        stats.dms_imported.values().map(Vec::len).sum::<usize>(),
        stats.posts_removed,
        stats.interactions_removed,
        stats.rejected,
        stats.follows_merged,
        stats.moderation_merged,
        stats.bookmarks_merged,
        stats.devices_added,
    );
}

/// Handle an incoming `DeviceSyncRequest`.
///
/// The request is served only if the remote NodeId derives from the local master key.
/// The exchange is limited to `DEVICE_SYNC_TIMEOUT`.
pub async fn handle_device_sync(
    storage: &Storage,
    identity: &SharedIdentity,
    gossip: &GossipService,
    app_handle: &AppHandle,
    conn: &Connection,
) -> Result<(), AcceptError> {
    let remote = conn.remote_id().to_string();
    if identity.read().await.device_index_of(&remote).is_none() {
        log::warn!(
            "[device-sync] rejected request from {}: not a device of the local user",
            short_id(&remote)
        );
        conn.close(VarInt::from_u32(CLOSE_FAILED), b"not an own device");
        return Err(AcceptError::from_err(std::io::Error::other(
            "device sync requested by a NodeId that is not an own device",
        )));
    }

    let sync = DeviceSync { storage, identity };
    let result = match tokio::time::timeout(DEVICE_SYNC_TIMEOUT, sync.respond(conn)).await {
        Ok(result) => result,
        Err(_) => Err("device sync: exchange timed out".into()),
    };
    match result {
        Ok(stats) => {
            log_stats("served", &remote, &stats);
            after_sync(storage, identity, gossip, app_handle, &stats).await;
            // The requester closes the connection after its last frame was acknowledged.
            let _ = tokio::time::timeout(STEP_TIMEOUT, conn.closed()).await;
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

/// Run a device sync with every other device in the `linked_devices` table.
/// A stored NodeId that does not derive from the local master key is skipped.
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
        if state
            .identity
            .read()
            .await
            .device_index_of(node_id)
            .is_none()
        {
            log::warn!(
                "[device-sync] skipping {}: not a device of the local user",
                short_id(node_id)
            );
            continue;
        }
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
    use crate::state::{Identity, device_node_id, test_support::loopback_endpoint};
    use iroh::SecretKey;
    use proscenium_types::{
        InteractionKind, PostTombstone, derive_transport_key, sign_delete_post, sign_interaction,
        sign_post,
    };
    use std::sync::Arc;

    fn signed_post(master: &[u8; 32], content: &str) -> Post {
        let mut post = Post {
            id: String::new(),
            author: SecretKey::from_bytes(master).public().to_string(),
            content: content.to_string(),
            timestamp: now_millis(),
            media: vec![],
            reply_to: None,
            reply_to_author: None,
            quote_of: None,
            quote_of_author: None,
            signature: String::new(),
        };
        sign_post(&mut post, &SecretKey::from_bytes(master));
        post
    }

    fn signed_like(master: &[u8; 32], target: &Post) -> Interaction {
        let mut like = Interaction {
            id: "ab".repeat(16),
            author: SecretKey::from_bytes(master).public().to_string(),
            kind: InteractionKind::Like,
            target_post_id: target.id.clone(),
            target_author: target.author.clone(),
            timestamp: now_millis(),
            signature: String::new(),
        };
        sign_interaction(&mut like, &SecretKey::from_bytes(master));
        like
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
        let request = recv.read_to_end(10_000).await?;
        let PeerRequest::DeviceSyncRequest = serde_json::from_slice(&request)? else {
            return Err("unexpected request".into());
        };
        drop(send);
        let sync = DeviceSync {
            storage: &storage,
            identity: &identity,
        };
        let stats = sync.respond(&conn).await?;
        let _ = tokio::time::timeout(STEP_TIMEOUT, conn.closed()).await;
        Ok(stats)
    }

    #[tokio::test]
    async fn device_sync_over_loopback_converges_both_devices() {
        let master = [1u8; 32];
        let other = [2u8; 32];
        let shared = |identity: Identity| Arc::new(tokio::sync::RwLock::new(identity));
        let id_a = shared(Identity::derive(master, 0));
        let id_b = shared(Identity::derive(master, 1));
        let me = id_a.read().await.master_pubkey.clone();
        let (node_a, node_b) = (device_node_id(&master, 0), device_node_id(&master, 1));
        let followed = SecretKey::from_bytes(&other).public().to_string();
        let muted = SecretKey::from_bytes(&[3u8; 32]).public().to_string();
        let bookmark = |state: &str, changed: u64| BookmarkEntry {
            post_id: "cd".repeat(16),
            created_at: 1,
            state: state.to_string(),
            last_changed_at: changed,
        };

        // Device A: a post, a follow, a removed bookmark, both devices.
        let a = Arc::new(Storage::open_in_memory().await.unwrap());
        let post = signed_post(&master, "from device a");
        a.insert_post(&post).await.unwrap();
        a.merge_follows_lww(
            &me,
            &[FollowEntry {
                pubkey: followed.clone(),
                followed_at: 1,
                state: "active".to_string(),
                last_changed_at: 100,
            }],
        )
        .await
        .unwrap();
        a.merge_bookmarks_lww(&[bookmark("removed", 200)])
            .await
            .unwrap();
        a.upsert_linked_device(&node_a, "Primary Device", true, true, 1)
            .await
            .unwrap();
        a.upsert_linked_device(&node_b, "Device 1", false, false, 2)
            .await
            .unwrap();
        // A post that device A deleted and device B still holds, and an old like that
        // device B removed and device A still holds.
        let mut deleted = signed_post(&master, "deleted on device a");
        deleted.timestamp = 1_000;
        sign_post(&mut deleted, &SecretKey::from_bytes(&master));
        let deletion = PostTombstone {
            id: deleted.id.clone(),
            author: me.clone(),
            signature: sign_delete_post(&deleted.id, &me, &SecretKey::from_bytes(&master)),
        };
        a.apply_post_tombstones(&me, std::slice::from_ref(&deletion), 1)
            .await
            .unwrap();
        let mut unliked = signed_like(&master, &signed_post(&other, "old target"));
        unliked.id = "ef".repeat(16);
        unliked.timestamp = 1_000;
        sign_interaction(&mut unliked, &SecretKey::from_bytes(&master));
        a.save_interaction(&unliked).await.unwrap();
        // A message from `followed` that only device A received.
        let received = dm(&me, &followed, &followed, &me, "hi from followed");
        assert!(
            a.receive_dm_message(&followed, &received, "hi")
                .await
                .unwrap()
        );

        // Device B: a like, a mute, the same bookmark in its older active state, and
        // only its own device row.
        let b = Arc::new(Storage::open_in_memory().await.unwrap());
        let like = signed_like(&master, &signed_post(&other, "target"));
        b.save_interaction(&like).await.unwrap();
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
        b.upsert_linked_device(&node_b, "Device 1", false, true, 2)
            .await
            .unwrap();
        b.insert_post(&deleted).await.unwrap();
        b.record_interaction_tombstone(&me, &unliked.id, 1)
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

        let ep_a = loopback_endpoint(
            SecretKey::from_bytes(&derive_transport_key(&master, 0)),
            PEER_ALPN,
        )
        .await;
        let ep_b = loopback_endpoint(
            SecretKey::from_bytes(&derive_transport_key(&master, 1)),
            PEER_ALPN,
        )
        .await;
        assert_eq!(ep_a.id().to_string(), node_a);
        let server = tokio::spawn(serve_one_sync(ep_a.clone(), a.clone(), id_a.clone()));

        let limit = Duration::from_secs(30);
        let conn = tokio::time::timeout(limit, ep_b.connect(ep_a.addr(), PEER_ALPN))
            .await
            .expect("connect timed out")
            .expect("connect");
        let sync_b = DeviceSync {
            storage: &b,
            identity: &id_b,
        };
        let stats_b = tokio::time::timeout(limit, sync_b.request(&conn))
            .await
            .expect("request timed out")
            .expect("request");
        conn.close(VarInt::from_u32(0), b"done");
        let stats_a = tokio::time::timeout(limit, server)
            .await
            .expect("responder timed out")
            .expect("responder panicked")
            .expect("responder");
        ep_b.close().await;
        ep_a.close().await;

        // B received A's post.
        assert_eq!(stats_b.posts_imported, 1);
        assert_eq!(stats_b.rejected, 0);
        assert_eq!(b.count_posts_by_author(&me).await.unwrap(), 1);
        // A received B's like.
        assert_eq!(stats_a.interactions_imported, 1);
        assert_eq!(stats_a.rejected, 0);
        assert_eq!(a.count_interactions_by_author(&me).await.unwrap(), 1);

        // Each device applied the deletion it lacked.
        assert_eq!(stats_b.posts_removed, 1);
        assert!(b.is_post_tombstoned(&me, &deleted.id).await.unwrap());
        assert_eq!(stats_a.interactions_removed, 1);
        assert!(a.is_interaction_tombstoned(&me, &unliked.id).await.unwrap());

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

        // B learned A's device row; A already held both.
        assert_eq!(stats_b.devices_added, 1);
        assert_eq!(stats_a.devices_added, 0);
        assert_eq!(b.get_other_device_node_ids().await.unwrap(), vec![node_a]);

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
    fn imported_post_must_be_own_valid_and_signed_by_the_master_key() {
        let master = [1u8; 32];
        let other = [2u8; 32];
        let me = SecretKey::from_bytes(&master).public().to_string();

        assert!(is_own_valid_post(&signed_post(&master, "a"), &me));

        // Another author, signed by that author's key.
        assert!(!is_own_valid_post(&signed_post(&other, "b"), &me));

        // Own author field, signature made by a foreign key.
        let mut forged = signed_post(&master, "c");
        sign_post(&mut forged, &SecretKey::from_bytes(&other));
        assert!(!is_own_valid_post(&forged, &me));

        // Content changed after signing: the id no longer matches.
        let mut tampered = signed_post(&master, "d");
        tampered.content.push('!');
        assert!(!is_own_valid_post(&tampered, &me));
    }

    #[test]
    fn imported_interaction_must_be_own_valid_and_signed_by_the_master_key() {
        let master = [1u8; 32];
        let me = SecretKey::from_bytes(&master).public().to_string();
        let target = signed_post(&[2u8; 32], "target");

        let like = signed_like(&master, &target);
        assert!(is_own_valid_interaction(&like, &me));

        let mut unsigned = like.clone();
        unsigned.signature = "00".repeat(64);
        assert!(!is_own_valid_interaction(&unsigned, &me));

        let mut bad_id = like.clone();
        bad_id.target_post_id = "not-an-id".to_string();
        assert!(!is_own_valid_interaction(&bad_id, &me));
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
