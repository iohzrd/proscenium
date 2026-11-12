//! Requester side of the peer sync protocol (`PeerRequest::Sync` on the peer ALPN).

use crate::ingestion::{IngestionManager, is_public};
use crate::storage::{ChangeError, RegistrationChange};
use iroh::endpoint::{Connection, RecvStream};
use proscenium_types::{
    Interaction, MAX_SYNC_IDS, MAX_SYNC_TOMBSTONES, PEER_ALPN, PeerRequest, Post, PostTombstone,
    SyncFrame, SyncMode, SyncRequest, SyncSummary, is_valid_id, post_ids_digest,
    sends_only_newer_interactions, short_id, validate_profile, verify_delete_post_signature,
    verify_interaction_signature, verify_linked_devices_announcement, verify_post_signature,
    verify_profile_signature,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::timeout;

/// Time allowed to connect to one NodeId.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// NodeIds of one user dialed per sync before the sync fails.
const MAX_DIAL_ATTEMPTS: usize = 3;
/// Time allowed to send the request and receive the summary.
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(15);
/// Time allowed to open the data stream and send the id list.
const ID_LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// Time allowed to receive one frame.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);
/// Time allowed for one sync from connect to the last frame.
const SYNC_TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_SUMMARY_BYTES: usize = 64 * 1024;
/// Largest accepted frame. Equal to the limit of the client's frame reader; a frame of
/// 200 posts of maximum size is about 8 MB.
const MAX_FRAME_BYTES: usize = 10_000_000;
/// Data frames accepted in one sync. A responder sends at most 200 posts per frame.
const MAX_SYNC_FRAMES: usize = 2_000;
/// Frames without an item accepted in one sync. Without this limit a responder holds the
/// sync permit for `SYNC_TOTAL_TIMEOUT` by sending one empty frame per `FRAME_TIMEOUT`.
const MAX_EMPTY_FRAMES: usize = 16;

/// Items checked between two yields to the scheduler. One signature verification takes
/// tens of microseconds and one frame can hold tens of thousands of items.
const VERIFY_CHUNK: usize = 64;

/// Keep the items for which `is_valid` returns true. Yields to the scheduler after every
/// `VERIFY_CHUNK` items, so the checks of one frame do not occupy a worker thread
/// without interruption.
async fn retain_valid<T>(items: Vec<T>, is_valid: impl Fn(&T) -> bool) -> Vec<T> {
    let mut valid = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 && index % VERIFY_CHUNK == 0 {
            tokio::task::yield_now().await;
        }
        if is_valid(&item) {
            valid.push(item);
        }
    }
    valid
}

/// Counts of one sync.
#[derive(Debug, Default)]
pub struct SyncStats {
    /// Posts inserted.
    pub posts: usize,
    /// Interactions inserted.
    pub interactions: usize,
    /// Stored posts deleted because of a received tombstone.
    pub deleted_posts: u64,
    /// Stored interactions deleted because the author's complete interaction set lacks
    /// them.
    pub deleted_interactions: u64,
    /// Items not stored: wrong author, failed validation or signature, or over a limit.
    pub rejected: usize,
    received_posts: usize,
    received_interactions: usize,
    received_tombstones: usize,
}

/// Entry in `IngestionManager::syncing`; removed when dropped.
struct SyncSlot<'a> {
    syncing: &'a Mutex<HashSet<String>>,
    pubkey: &'a str,
}

impl<'a> SyncSlot<'a> {
    /// `None` if a sync of `pubkey` is already queued or running.
    fn acquire(syncing: &'a Mutex<HashSet<String>>, pubkey: &'a str) -> Option<Self> {
        let inserted = syncing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pubkey.to_string());
        inserted.then_some(Self { syncing, pubkey })
    }
}

impl Drop for SyncSlot<'_> {
    fn drop(&mut self) {
        self.syncing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(self.pubkey);
    }
}

/// Read one `[u32 big-endian length][payload]` frame. `None` for a zero length.
async fn read_frame(recv: &mut RecvStream) -> anyhow::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_FRAME_BYTES {
        anyhow::bail!("frame of {len} bytes exceeds {MAX_FRAME_BYTES}");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

impl IngestionManager {
    /// Sync `pubkey` from one of its devices and log the result. Returns without
    /// syncing if a sync of `pubkey` is already queued or running. Waits for one of the
    /// `sync.max_concurrent_syncs` permits.
    pub(crate) async fn sync_user(self: &Arc<Self>, pubkey: &str) {
        let Some(_slot) = SyncSlot::acquire(&self.syncing, pubkey) else {
            return;
        };
        // The semaphore is never closed.
        let Ok(_permit) = self.sync_permits.acquire().await else {
            return;
        };

        let mut stats = SyncStats::default();
        let result = timeout(SYNC_TOTAL_TIMEOUT, self.sync_from_peer(pubkey, &mut stats)).await;
        if stats.posts > 0
            || stats.interactions > 0
            || stats.deleted_posts > 0
            || stats.deleted_interactions > 0
        {
            tracing::info!(
                "[sync] {}: stored {} posts and {} interactions, deleted {} posts and {} interactions",
                short_id(pubkey),
                stats.posts,
                stats.interactions,
                stats.deleted_posts,
                stats.deleted_interactions
            );
        }
        if stats.rejected > 0 {
            tracing::warn!(
                "[sync] {}: {} received items were not stored",
                short_id(pubkey),
                stats.rejected
            );
        }
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("[sync] sync of {} failed: {e}", short_id(pubkey)),
            Err(_) => tracing::warn!(
                "[sync] sync of {} exceeded {}s",
                short_id(pubkey),
                SYNC_TOTAL_TIMEOUT.as_secs()
            ),
        }
    }

    /// Connect to the first reachable stored NodeId of `pubkey`, at most
    /// `MAX_DIAL_ATTEMPTS` of them, each with `CONNECT_TIMEOUT`.
    async fn connect_to_user(&self, pubkey: &str) -> anyhow::Result<Connection> {
        let mut last_error = anyhow::anyhow!("no NodeId stored");
        for node_id in self
            .stored_node_ids(pubkey)
            .await
            .into_iter()
            .take(MAX_DIAL_ATTEMPTS)
        {
            match timeout(CONNECT_TIMEOUT, self.endpoint.connect(node_id, PEER_ALPN)).await {
                Ok(Ok(conn)) => return Ok(conn),
                Ok(Err(e)) => {
                    last_error = anyhow::anyhow!("connect to {} failed: {e}", node_id.fmt_short());
                }
                Err(_) => {
                    last_error = anyhow::anyhow!(
                        "connect to {} exceeded {}s",
                        node_id.fmt_short(),
                        CONNECT_TIMEOUT.as_secs()
                    );
                }
            }
        }
        Err(last_error)
    }

    async fn sync_from_peer(
        self: &Arc<Self>,
        pubkey: &str,
        stats: &mut SyncStats,
    ) -> anyhow::Result<()> {
        let conn = self.connect_to_user(pubkey).await?;
        let result = self.sync_over_connection(&conn, pubkey, stats).await;
        // The responder waits for this close before it ends its handler.
        conn.close(0u32.into(), b"done");
        result
    }

    async fn sync_over_connection(
        self: &Arc<Self>,
        conn: &Connection,
        pubkey: &str,
        stats: &mut SyncStats,
    ) -> anyhow::Result<()> {
        let cursor = self
            .storage
            .get_sync_cursor(pubkey, self.min_timestamp(), self.limits.max_posts_per_user)
            .await?;
        let mut sorted_ids = cursor.post_ids.clone();
        let request = PeerRequest::Sync(SyncRequest {
            author: pubkey.to_string(),
            post_count: cursor.post_ids.len() as u64,
            interaction_count: cursor.interaction_count,
            newest_timestamp: cursor.newest_post_timestamp,
            newest_interaction_timestamp: cursor.newest_interaction_timestamp,
            post_ids_digest: post_ids_digest(&mut sorted_ids),
            tombstone_count: cursor.tombstone_count,
            since: cursor.since,
            announcement: None,
        });
        let request_bytes = serde_json::to_vec(&request)?;

        // First stream: request out, summary in.
        let summary: SyncSummary = timeout(SUMMARY_TIMEOUT, async {
            let (mut send, mut recv) = conn.open_bi().await?;
            send.write_all(&request_bytes).await?;
            send.finish()?;
            let bytes = recv.read_to_end(MAX_SUMMARY_BYTES).await?;
            anyhow::Ok(serde_json::from_slice::<SyncSummary>(&bytes)?)
        })
        .await
        .map_err(|_| anyhow::anyhow!("no summary within {}s", SUMMARY_TIMEOUT.as_secs()))??;

        // The signed device list replaces the stored device NodeIds if its version is higher.
        if let Some(announcement) = &summary.announcement {
            if announcement.master_pubkey != pubkey {
                tracing::warn!(
                    "[sync] {}: device announcement names another user",
                    short_id(pubkey)
                );
            } else if let Err(reason) = verify_linked_devices_announcement(announcement) {
                tracing::warn!(
                    "[sync] {}: invalid device announcement: {reason}",
                    short_id(pubkey)
                );
            } else {
                match self.storage.cache_announcement(announcement).await {
                    Ok(_) => {}
                    Err(ChangeError::Rejected(rejection)) => tracing::warn!(
                        "[sync] {}: device announcement not stored: {rejection}",
                        short_id(pubkey)
                    ),
                    Err(ChangeError::Storage(e)) => return Err(e),
                }
            }
        }

        if let Some(profile) = &summary.profile {
            let verified =
                validate_profile(profile).and_then(|()| verify_profile_signature(profile, pubkey));
            match verified {
                Ok(()) => {
                    let change = RegistrationChange::PeerProfile {
                        master_pubkey: pubkey,
                        profile,
                    };
                    match self.apply_registration_change(change).await {
                        // The profile ended the public registration; nothing is fetched.
                        Ok(outcome) if !is_public(&outcome) => return Ok(()),
                        Ok(_) => {}
                        Err(ChangeError::Rejected(rejection)) => {
                            anyhow::bail!("profile not applied: {rejection}")
                        }
                        Err(ChangeError::Storage(e)) => return Err(e),
                    }
                }
                Err(reason) => tracing::warn!(
                    "[sync] {}: invalid profile in summary: {reason}",
                    short_id(pubkey)
                ),
            }
        }

        if summary.mode == SyncMode::UpToDate {
            return Ok(());
        }

        // Second stream: the id list for `NeedIdDiff` (the newest `MAX_SYNC_IDS` ids),
        // nothing for `TimestampCatchUp`. The responder's `accept_bi` returns only after
        // data or the finish of the stream was sent.
        let known_ids: Vec<&String> = cursor.post_ids.iter().take(MAX_SYNC_IDS).collect();
        let id_list = match summary.mode {
            SyncMode::NeedIdDiff => Some(serde_json::to_vec(&known_ids)?),
            _ => None,
        };
        let mut recv = timeout(ID_LIST_TIMEOUT, async {
            let (mut send, recv) = conn.open_bi().await?;
            if let Some(id_list) = &id_list {
                send.write_all(id_list).await?;
            }
            send.finish()?;
            anyhow::Ok(recv)
        })
        .await
        .map_err(|_| anyhow::anyhow!("id list not sent within {}s", ID_LIST_TIMEOUT.as_secs()))??;

        // The responder sends every interaction of the author, not only newer ones, when
        // the count difference is not explained by interactions after the newest stored
        // timestamp. The ids of that set identify the interactions the author deleted.
        let full_interaction_set = summary.server_interaction_count != cursor.interaction_count
            && !sends_only_newer_interactions(
                summary.server_interaction_count,
                cursor.interaction_count,
                summary.interactions_after_count,
            );
        let mut received_interaction_ids: Vec<String> = Vec::new();

        let (mut frames, mut empty_frames) = (0usize, 0usize);
        loop {
            let frame = timeout(FRAME_TIMEOUT, read_frame(&mut recv))
                .await
                .map_err(|_| anyhow::anyhow!("no frame within {}s", FRAME_TIMEOUT.as_secs()))??;
            let Some(frame) = frame else {
                break;
            };
            let frame = serde_json::from_slice::<SyncFrame>(&frame)?;
            frames += 1;
            let items = match &frame {
                SyncFrame::Tombstones(items) => items.len(),
                SyncFrame::Posts(items) => items.len(),
                SyncFrame::Interactions(items) => items.len(),
            };
            if items == 0 {
                empty_frames += 1;
            }
            if frames > MAX_SYNC_FRAMES || empty_frames > MAX_EMPTY_FRAMES {
                anyhow::bail!(
                    "peer sent more than {MAX_SYNC_FRAMES} frames or {MAX_EMPTY_FRAMES} empty frames"
                );
            }
            match frame {
                SyncFrame::Tombstones(tombstones) => {
                    self.store_synced_tombstones(pubkey, tombstones, stats)
                        .await?;
                }
                SyncFrame::Posts(posts) => {
                    self.store_synced_posts(pubkey, posts, stats).await?;
                }
                SyncFrame::Interactions(interactions) => {
                    if full_interaction_set {
                        received_interaction_ids.extend(interactions.iter().map(|i| i.id.clone()));
                    }
                    self.store_synced_interactions(pubkey, interactions, stats)
                        .await?;
                }
            }
        }

        // The stream ended with its end marker and carried the author's complete set.
        if full_interaction_set
            && received_interaction_ids.len() as u64 == summary.server_interaction_count
        {
            stats.deleted_interactions = self
                .storage
                .delete_interactions_not_in(pubkey, &received_interaction_ids)
                .await?;
        }
        Ok(())
    }

    /// Verify the tombstones of one frame; delete the named posts and store the
    /// tombstones. Fails once more than `MAX_SYNC_TOMBSTONES` were received in the sync.
    async fn store_synced_tombstones(
        &self,
        pubkey: &str,
        tombstones: Vec<PostTombstone>,
        stats: &mut SyncStats,
    ) -> anyhow::Result<()> {
        stats.received_tombstones = stats.received_tombstones.saturating_add(tombstones.len());
        if stats.received_tombstones > MAX_SYNC_TOMBSTONES {
            anyhow::bail!("peer sent more than {MAX_SYNC_TOMBSTONES} tombstones");
        }
        let received = tombstones.len();
        let verified = retain_valid(tombstones, |t| {
            t.author == pubkey
                && is_valid_id(&t.id)
                && verify_delete_post_signature(&t.id, &t.author, &t.signature).is_ok()
        })
        .await;
        stats.rejected += received - verified.len();
        let ids: Vec<&str> = verified.iter().map(|t| t.id.as_str()).collect();
        stats.deleted_posts += self.storage.apply_tombstones(pubkey, &ids).await?;
        Ok(())
    }

    /// Verify the posts of one frame and insert them. Fails once more than
    /// `max_posts_per_user` posts were received in the sync.
    async fn store_synced_posts(
        &self,
        pubkey: &str,
        posts: Vec<Post>,
        stats: &mut SyncStats,
    ) -> anyhow::Result<()> {
        let max_posts = self.limits.max_posts_per_user;
        stats.received_posts = stats.received_posts.saturating_add(posts.len());
        if stats.received_posts > max_posts as usize {
            anyhow::bail!("peer sent more than {max_posts} posts");
        }
        let received = posts.len();
        let verified = retain_valid(posts, |post| {
            self.check_post_fields(post, pubkey).is_ok() && verify_post_signature(post).is_ok()
        })
        .await;
        stats.rejected += received - verified.len();
        let count = self
            .storage
            .insert_posts(pubkey, &verified, max_posts)
            .await?;
        stats.posts += count.stored;
        stats.rejected += count.over_limit;
        Ok(())
    }

    /// Verify the interactions of one frame and insert them. Fails once more than
    /// `max_interactions_per_user` interactions were received in the sync.
    async fn store_synced_interactions(
        &self,
        pubkey: &str,
        interactions: Vec<Interaction>,
        stats: &mut SyncStats,
    ) -> anyhow::Result<()> {
        let max_interactions = self.limits.max_interactions_per_user;
        stats.received_interactions = stats
            .received_interactions
            .saturating_add(interactions.len());
        if stats.received_interactions > max_interactions as usize {
            anyhow::bail!("peer sent more than {max_interactions} interactions");
        }
        let received = interactions.len();
        let verified = retain_valid(interactions, |interaction| {
            self.check_interaction_fields(interaction, pubkey).is_ok()
                && verify_interaction_signature(interaction).is_ok()
        })
        .await;
        stats.rejected += received - verified.len();
        let count = self
            .storage
            .insert_interactions(pubkey, &verified, max_interactions)
            .await?;
        stats.interactions += count.stored;
        stats.rejected += count.over_limit;
        Ok(())
    }
}
