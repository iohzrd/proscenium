mod client;
mod orchestration;
mod processing;
mod server;

pub use orchestration::sync_one_peer;
pub use server::handle_sync;

use proscenium_types::{Interaction, Post, Profile, SyncMode};

/// Result returned from a sync operation.
pub struct SyncResult {
    pub posts: Vec<Post>,
    pub interactions: Vec<Interaction>,
    pub profile: Option<Profile>,
    pub remote_post_count: u64,
    pub mode: SyncMode,
    pub active_stage: Option<proscenium_types::StageAnnouncement>,
}
