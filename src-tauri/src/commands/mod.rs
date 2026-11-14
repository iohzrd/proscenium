mod blobs;
mod calls;
mod devices;
mod dm;
mod follow_requests;
mod identity;
mod interactions;
mod moderation;
mod notifications;
mod posts;
mod preferences;
mod profile;
pub(crate) mod servers;
mod social;
mod stage;
pub(crate) mod sync;
mod uploads;

pub use blobs::*;
pub use calls::*;
pub use devices::*;
pub use dm::*;
pub use follow_requests::*;
pub use identity::*;
pub use interactions::*;
pub use moderation::*;
pub use notifications::*;
pub use posts::*;
pub use preferences::*;
pub use profile::*;
pub use servers::*;
pub use social::*;
pub use stage::*;
pub use sync::{get_sync_status, sync_all_peers, sync_posts};
pub use uploads::*;

use crate::error::CmdResult;
use proscenium_types::{is_valid_id, is_valid_pubkey};

/// Sorts after every post id (ids are lowercase hex digits). An `after` cursor given
/// without an id uses it, so only rows with a greater timestamp are returned.
const AFTER_EVERY_ID: &str = "\u{10FFFF}";

/// Return an error unless `id` has the format of a post or interaction id.
pub(crate) fn require_id(id: &str) -> CmdResult<()> {
    if is_valid_id(id) {
        Ok(())
    } else {
        Err("invalid id".into())
    }
}

/// Return an error unless `pubkey` is a canonical public key string.
pub(crate) fn require_pubkey(pubkey: &str) -> CmdResult<()> {
    if is_valid_pubkey(pubkey) {
        Ok(())
    } else {
        Err("invalid public key".into())
    }
}

/// Cursor for a page ordered by `(timestamp, id)` descending, from the `before` and
/// `before_id` command arguments. Without `before_id` the page starts at the first row
/// with a smaller timestamp.
pub(crate) fn before_cursor(
    before: Option<u64>,
    before_id: Option<String>,
) -> Option<(u64, String)> {
    before.map(|timestamp| (timestamp, before_id.unwrap_or_default()))
}

/// Cursor for a page ordered by `(timestamp, id)` ascending, from the `after` and
/// `after_id` command arguments. Without `after_id` the page starts at the first row
/// with a greater timestamp.
pub(crate) fn after_cursor(after: Option<u64>, after_id: Option<String>) -> Option<(u64, String)> {
    after.map(|timestamp| {
        (
            timestamp,
            after_id.unwrap_or_else(|| AFTER_EVERY_ID.to_string()),
        )
    })
}

/// Borrow an owned cursor in the form the storage functions take.
pub(crate) fn as_storage_cursor(
    cursor: &Option<(u64, String)>,
) -> Option<crate::storage::Cursor<'_>> {
    cursor
        .as_ref()
        .map(|(timestamp, id)| (*timestamp, id.as_str()))
}
