use crate::constants::{
    SYNC_MAX_INTERACTIONS_PER_RUN, SYNC_MAX_POSTS_PER_RUN, SYNC_RETURNED_POSTS_MAX,
};
use crate::error::AppError;
use crate::ingest::{
    InteractionOutcome, verify_and_apply_post_tombstones, verify_and_store_interaction,
    verify_and_store_post,
};
use crate::storage::Storage;
use proscenium_types::{MAX_SYNC_TOMBSTONES, Post, SyncFrame};
use tauri::{AppHandle, Runtime};

/// Verifies and stores the frames of one sync stream as they arrive.
///
/// Every item must be authored and signed by `pubkey`. Items that fail a check are
/// skipped; exceeding an item cap is an error.
pub(super) struct FrameIngest<'a, R: Runtime> {
    storage: &'a Storage,
    pubkey: &'a str,
    label: &'a str,
    my_id: &'a str,
    app_handle: &'a AppHandle<R>,
    posts_received: usize,
    interactions_received: usize,
    tombstones_received: usize,
    /// Number of new post rows inserted.
    pub posts_stored: usize,
    /// Number of post rows deleted by tombstones.
    pub posts_removed: u64,
    /// Number of tombstone rows inserted (tombstones that were not stored before).
    pub tombstones_added: u64,
    /// The newest newly stored posts, at most `2 * SYNC_RETURNED_POSTS_MAX` between trims.
    new_posts: Vec<Post>,
    /// Ids of the valid interactions received. `Some` only while the responder sends its
    /// complete interaction set, which the caller then compares with the local rows.
    interaction_ids: Option<Vec<String>>,
}

impl<'a, R: Runtime> FrameIngest<'a, R> {
    pub fn new(
        storage: &'a Storage,
        pubkey: &'a str,
        label: &'a str,
        my_id: &'a str,
        app_handle: &'a AppHandle<R>,
        collect_interaction_ids: bool,
    ) -> Self {
        Self {
            storage,
            pubkey,
            label,
            my_id,
            app_handle,
            posts_received: 0,
            interactions_received: 0,
            tombstones_received: 0,
            posts_stored: 0,
            posts_removed: 0,
            tombstones_added: 0,
            new_posts: Vec::new(),
            interaction_ids: collect_interaction_ids.then(Vec::new),
        }
    }

    /// Verify and store the items of one frame.
    /// Returns an error if the stream has carried more items than a requester accepts.
    pub async fn process(&mut self, frame: SyncFrame) -> Result<(), AppError> {
        match frame {
            SyncFrame::Tombstones(tombstones) => {
                self.tombstones_received =
                    self.tombstones_received.saturating_add(tombstones.len());
                if self.tombstones_received > MAX_SYNC_TOMBSTONES {
                    return Err(AppError::Other(format!(
                        "sync stream exceeds {MAX_SYNC_TOMBSTONES} tombstones"
                    )));
                }
                let outcome = verify_and_apply_post_tombstones(
                    self.storage,
                    self.pubkey,
                    &tombstones,
                    self.label,
                )
                .await;
                self.posts_removed += outcome.posts_removed;
                self.tombstones_added += outcome.tombstones_added;
            }
            SyncFrame::Posts(posts) => {
                self.posts_received = self.posts_received.saturating_add(posts.len());
                if self.posts_received > SYNC_MAX_POSTS_PER_RUN {
                    return Err(AppError::Other(format!(
                        "sync stream exceeds {SYNC_MAX_POSTS_PER_RUN} posts"
                    )));
                }
                for post in posts {
                    if post.author != self.pubkey {
                        continue;
                    }
                    if verify_and_store_post(
                        self.storage,
                        &post,
                        self.label,
                        self.my_id,
                        self.app_handle,
                    )
                    .await
                    {
                        self.posts_stored += 1;
                        self.new_posts.push(post);
                    }
                }
                if self.new_posts.len() > 2 * SYNC_RETURNED_POSTS_MAX {
                    self.trim_new_posts();
                }
            }
            SyncFrame::Interactions(interactions) => {
                self.interactions_received = self
                    .interactions_received
                    .saturating_add(interactions.len());
                if self.interactions_received > SYNC_MAX_INTERACTIONS_PER_RUN {
                    return Err(AppError::Other(format!(
                        "sync stream exceeds {SYNC_MAX_INTERACTIONS_PER_RUN} interactions"
                    )));
                }
                for interaction in interactions {
                    let outcome = verify_and_store_interaction(
                        self.storage,
                        &interaction,
                        self.pubkey,
                        self.label,
                        self.my_id,
                        self.app_handle,
                    )
                    .await;
                    if outcome != InteractionOutcome::Rejected
                        && let Some(ids) = self.interaction_ids.as_mut()
                    {
                        ids.push(interaction.id);
                    }
                }
            }
        }
        Ok(())
    }

    /// Keep the `SYNC_RETURNED_POSTS_MAX` newly stored posts with the highest timestamps,
    /// newest first.
    fn trim_new_posts(&mut self) {
        self.new_posts
            .sort_unstable_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.id.cmp(&a.id)));
        self.new_posts.truncate(SYNC_RETURNED_POSTS_MAX);
    }

    /// The newest newly stored posts (at most `SYNC_RETURNED_POSTS_MAX`, newest first).
    pub fn take_new_posts(&mut self) -> Vec<Post> {
        self.trim_new_posts();
        std::mem::take(&mut self.new_posts)
    }

    /// Ids of the valid interactions received, if they were collected.
    pub fn take_interaction_ids(&mut self) -> Option<Vec<String>> {
        self.interaction_ids.take()
    }
}
