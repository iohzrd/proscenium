use super::client::sync_from_peer;
use crate::constants::{MAX_NODE_IDS_TRIED, SYNC_PEER_TIMEOUT};
use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::{Endpoint, EndpointId};
use proscenium_types::{Post, is_valid_pubkey, short_id};
use tauri::AppHandle;
use tokio::time::Instant;

/// Stats returned from `sync_one_peer`.
pub struct SyncOneResult {
    /// Number of new post rows inserted.
    pub stored: usize,
    /// The newest newly stored posts (at most `SYNC_RETURNED_POSTS_MAX`, newest first).
    pub posts: Vec<Post>,
    /// Number of posts the responder reported for the author.
    pub remote_post_count: u64,
}

/// Sync the content of the user `pubkey` from one of its cached transport NodeIds.
///
/// Connects to at most `MAX_NODE_IDS_TRIED` NodeIds in order and runs for at most
/// `SYNC_PEER_TIMEOUT` in total (plus one frame timeout). Returns the result of the
/// first NodeId that answered with a usable summary. Nothing is requested for a muted
/// or blocked user.
pub async fn sync_one_peer(
    endpoint: &Endpoint,
    storage: &Storage,
    identity: &SharedIdentity,
    pubkey: &str,
    app_handle: &AppHandle,
    label: &str,
) -> Result<SyncOneResult, AppError> {
    if !is_valid_pubkey(pubkey) {
        return Err(AppError::Other("invalid pubkey".into()));
    }
    if pubkey == identity.read().await.master_pubkey {
        return Err(AppError::Other("cannot sync the local user".into()));
    }
    if storage.is_hidden(pubkey).await? {
        return Ok(SyncOneResult {
            stored: 0,
            posts: Vec::new(),
            remote_post_count: 0,
        });
    }

    let node_ids = storage.get_peer_transport_node_ids(pubkey).await?;
    if node_ids.is_empty() {
        return Err(AppError::Other(format!(
            "no cached transport NodeId for {}",
            short_id(pubkey)
        )));
    }

    let deadline = Instant::now() + SYNC_PEER_TIMEOUT;
    let mut last_err = String::from("no usable transport NodeId");
    let mut tried = 0usize;
    for node_id in node_ids.iter().take(MAX_NODE_IDS_TRIED) {
        if Instant::now() >= deadline {
            last_err = "time limit reached".to_string();
            break;
        }
        let target: EndpointId = match node_id.parse() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("[{label}] bad transport NodeId {}: {e}", short_id(node_id));
                continue;
            }
        };
        tried += 1;
        match sync_from_peer(
            endpoint, storage, identity, target, pubkey, app_handle, label, deadline,
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(e) => {
                log::debug!("[{label}] sync via {} failed: {e}", short_id(node_id));
                last_err = e.to_string();
            }
        }
    }

    Err(AppError::Other(format!(
        "sync failed for {} (tried {tried} node(s)): {last_err}",
        short_id(pubkey),
    )))
}

/// Fetch the own posts, interactions and post deletions of the local user from the other
/// device `target`, within `SYNC_PEER_TIMEOUT`. See `sync_from_peer` for what differs
/// from the sync of another user.
pub async fn sync_from_own_device(
    endpoint: &Endpoint,
    storage: &Storage,
    identity: &SharedIdentity,
    target: EndpointId,
    app_handle: &AppHandle,
) -> Result<SyncOneResult, AppError> {
    let my_id = identity.read().await.master_pubkey.clone();
    let deadline = Instant::now() + SYNC_PEER_TIMEOUT;
    sync_from_peer(
        endpoint,
        storage,
        identity,
        target,
        &my_id,
        app_handle,
        "device-sync",
        deadline,
    )
    .await
}
