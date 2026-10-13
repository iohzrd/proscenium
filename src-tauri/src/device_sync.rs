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
//! receiver's vector, and all follow, moderation and bookmark rows unless both vectors
//! carry the same `lww_digest`. Double Ratchet sessions are never sent: two devices
//! advancing copies of one session would reuse message keys.

use crate::constants::BATCH_SIZE;
use crate::error::AppError;
use crate::framing::{read_frame, write_frame};
use crate::gossip::GossipService;
use crate::state::{AppState, SharedIdentity};
use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, PublicKey, SecretKey,
    endpoint::{Connection, RecvStream, SendStream, VarInt},
    protocol::AcceptError,
};
use proscenium_types::{
    BookmarkEntry, DeviceSyncFrame, DeviceSyncVector, FollowEntry, Interaction, MAX_LINKED_DEVICES,
    ModerationEntry, PEER_ALPN, PeerRequest, Post, derive_signing_key, short_id,
    validate_interaction, validate_post, verify_interaction_signature, verify_post_signature,
};
use sha2::{Digest, Sha256};
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
/// Number of signing key indexes, counted back from the current one, whose signatures
/// are accepted on imported posts and interactions.
const MAX_SIGNING_KEY_HISTORY: u32 = 64;
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
    /// Signing key index in the other device's vector.
    pub peer_signing_key_index: u32,
}

/// Public signing keys for the indexes up to `signing_key_index`, at most
/// `MAX_SIGNING_KEY_HISTORY` of them. Own posts made before a key rotation carry a
/// signature of an earlier index.
fn own_signing_pubkeys(
    master_secret_key_bytes: &[u8; 32],
    signing_key_index: u32,
) -> Vec<PublicKey> {
    let first = signing_key_index.saturating_sub(MAX_SIGNING_KEY_HISTORY - 1);
    (first..=signing_key_index)
        .rev()
        .map(|index| {
            SecretKey::from_bytes(&derive_signing_key(master_secret_key_bytes, index)).public()
        })
        .collect()
}

/// True if `post` is authored by `master_pubkey`, passes `validate_post`, and carries a
/// signature of one of `signers`.
fn is_own_valid_post(post: &Post, master_pubkey: &str, signers: &[PublicKey]) -> bool {
    post.author == master_pubkey
        && validate_post(post).is_ok()
        && signers
            .iter()
            .any(|signer| verify_post_signature(post, signer).is_ok())
}

/// True if `interaction` is authored by `master_pubkey`, passes `validate_interaction`,
/// and carries a signature of one of `signers`.
fn is_own_valid_interaction(
    interaction: &Interaction,
    master_pubkey: &str,
    signers: &[PublicKey],
) -> bool {
    interaction.author == master_pubkey
        && validate_interaction(interaction).is_ok()
        && signers
            .iter()
            .any(|signer| verify_interaction_signature(interaction, signer).is_ok())
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
        let (master_pubkey, signing_key_index) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.signing_key_index)
        };
        let lww = self.storage.load_lww_state(&master_pubkey).await?;
        let vector = self
            .storage
            .build_device_sync_vector(&master_pubkey, &lww, signing_key_index)
            .await?;
        Ok((master_pubkey, lww, vector))
    }

    /// Apply the parts of the other device's vector that are not row data: record its
    /// signing key index, and store its device entries that are not stored yet and whose
    /// NodeId derives from the local master key.
    async fn apply_peer_vector(
        &self,
        peer: &DeviceSyncVector,
        stats: &mut DeviceSyncStats,
    ) -> Result<(), AppError> {
        stats.peer_signing_key_index = peer.signing_key_index;
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

        write_end_frame(send).await
    }

    /// Read data frames until the zero-length frame and store their content.
    ///
    /// A post or interaction is stored only if it is authored by the local user, passes
    /// validation and carries a signature of a signing key of this identity, up to the
    /// higher of the two devices' signing key indexes. At most `MAX_SYNC_FRAMES` frames
    /// and `MAX_SYNC_ITEMS` items are accepted.
    async fn import_frames(
        &self,
        recv: &mut RecvStream,
        master_pubkey: &str,
        stats: &mut DeviceSyncStats,
    ) -> Result<(), AppError> {
        let signers = {
            let id = self.identity.read().await;
            own_signing_pubkeys(
                &id.master_secret_key_bytes,
                id.signing_key_index.max(stats.peer_signing_key_index),
            )
        };
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
                        if !is_own_valid_post(post, master_pubkey, &signers)
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
                        if !is_own_valid_interaction(interaction, master_pubkey, &signers)
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
/// Raises the local signing key index if the other device reported a higher one, emits
/// `feed-updated` if posts were imported, and broadcasts the device announcement if the
/// device list or the signing key changed.
async fn after_sync(
    storage: &Storage,
    identity: &SharedIdentity,
    gossip: &GossipService,
    app_handle: &AppHandle,
    stats: &DeviceSyncStats,
) {
    let current_index = identity.read().await.signing_key_index;
    let mut signing_key_raised = false;
    if stats.peer_signing_key_index > current_index {
        match crate::setup::raise_signing_key_index(
            app_handle,
            identity,
            stats.peer_signing_key_index,
        )
        .await
        {
            Ok(_) => {
                log::info!(
                    "[device-sync] signing key index raised from {current_index} to {}",
                    stats.peer_signing_key_index
                );
                signing_key_raised = true;
            }
            Err(e) => log::warn!("[device-sync] signing key index not raised: {e}"),
        }
    }

    if stats.posts_imported > 0 {
        let _ = app_handle.emit("feed-updated", ());
    }
    if stats.devices_added == 0 && !signing_key_raised {
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
        "[device-sync] {role} sync with {}: posts={}, interactions={}, rejected={}, follows={}, moderation={}, bookmarks={}, devices={}",
        short_id(node_id),
        stats.posts_imported,
        stats.interactions_imported,
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
    use proscenium_types::{
        InteractionKind, derive_transport_key, now_millis, sign_interaction, sign_post,
    };
    use std::sync::Arc;

    fn signed_post(master: &[u8; 32], signing_index: u32, content: &str) -> Post {
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
        let key = SecretKey::from_bytes(&derive_signing_key(master, signing_index));
        sign_post(&mut post, &key);
        post
    }

    fn signed_like(master: &[u8; 32], signing_index: u32, target: &Post) -> Interaction {
        let mut like = Interaction {
            id: "ab".repeat(16),
            author: SecretKey::from_bytes(master).public().to_string(),
            kind: InteractionKind::Like,
            target_post_id: target.id.clone(),
            target_author: target.author.clone(),
            timestamp: now_millis(),
            signature: String::new(),
        };
        let key = SecretKey::from_bytes(&derive_signing_key(master, signing_index));
        sign_interaction(&mut like, &key);
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
        // Device A is the primary device and has rotated its signing key once.
        let id_a = shared(Identity::derive(master, 0, 1, 0));
        let id_b = shared(Identity::derive(master, 1, 0, 0));
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

        // Device A: a post signed with index 1, a follow, a removed bookmark, both devices.
        let a = Arc::new(Storage::open_in_memory().await.unwrap());
        let post = signed_post(&master, 1, "from device a");
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

        // Device B: a like, a mute, the same bookmark in its older active state, and
        // only its own device row.
        let b = Arc::new(Storage::open_in_memory().await.unwrap());
        let like = signed_like(&master, 0, &signed_post(&other, 0, "target"));
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

        // B received A's post although B's own signing key index is lower.
        assert_eq!(stats_b.posts_imported, 1);
        assert_eq!(stats_b.rejected, 0);
        assert_eq!(stats_b.peer_signing_key_index, 1);
        assert_eq!(b.count_posts_by_author(&me).await.unwrap(), 1);
        // A received B's like.
        assert_eq!(stats_a.interactions_imported, 1);
        assert_eq!(stats_a.rejected, 0);
        assert_eq!(a.count_interactions_by_author(&me).await.unwrap(), 1);

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

    #[test]
    fn imported_post_must_be_own_valid_and_signed_by_an_own_key() {
        let master = [1u8; 32];
        let me = SecretKey::from_bytes(&master).public().to_string();
        let signers = own_signing_pubkeys(&master, 2);
        assert_eq!(signers.len(), 3);

        // Signed with the current index and with an index from before a rotation.
        assert!(is_own_valid_post(
            &signed_post(&master, 2, "a"),
            &me,
            &signers
        ));
        assert!(is_own_valid_post(
            &signed_post(&master, 0, "b"),
            &me,
            &signers
        ));
        // Signed with an index that the local device has not reached.
        assert!(!is_own_valid_post(
            &signed_post(&master, 3, "c"),
            &me,
            &signers
        ));

        // Another author, signed by that author's key.
        let other = [2u8; 32];
        assert!(!is_own_valid_post(
            &signed_post(&other, 0, "d"),
            &me,
            &signers
        ));

        // Own author field, signature made by a foreign key.
        let mut forged = signed_post(&master, 0, "e");
        let foreign_key = SecretKey::from_bytes(&derive_signing_key(&other, 0));
        sign_post(&mut forged, &foreign_key);
        assert!(!is_own_valid_post(&forged, &me, &signers));

        // Content changed after signing: the id no longer matches.
        let mut tampered = signed_post(&master, 2, "f");
        tampered.content.push('!');
        assert!(!is_own_valid_post(&tampered, &me, &signers));
    }

    #[test]
    fn imported_interaction_must_be_own_valid_and_signed_by_an_own_key() {
        let master = [1u8; 32];
        let me = SecretKey::from_bytes(&master).public().to_string();
        let signers = own_signing_pubkeys(&master, 0);
        let target = signed_post(&[2u8; 32], 0, "target");

        let mut like = Interaction {
            id: "ab".repeat(16),
            author: me.clone(),
            kind: InteractionKind::Like,
            target_post_id: target.id.clone(),
            target_author: target.author.clone(),
            timestamp: now_millis(),
            signature: String::new(),
        };
        sign_interaction(
            &mut like,
            &SecretKey::from_bytes(&derive_signing_key(&master, 0)),
        );
        assert!(is_own_valid_interaction(&like, &me, &signers));

        let mut unsigned = like.clone();
        unsigned.signature = "00".repeat(64);
        assert!(!is_own_valid_interaction(&unsigned, &me, &signers));

        let mut bad_id = like.clone();
        bad_id.target_post_id = "not-an-id".to_string();
        assert!(!is_own_valid_interaction(&bad_id, &me, &signers));
    }

    #[test]
    fn signing_key_history_is_bounded() {
        let master = [1u8; 32];
        let signers = own_signing_pubkeys(&master, 1_000);
        assert_eq!(signers.len(), MAX_SIGNING_KEY_HISTORY as usize);
        let current = SecretKey::from_bytes(&derive_signing_key(&master, 1_000)).public();
        assert_eq!(signers.first(), Some(&current));
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
