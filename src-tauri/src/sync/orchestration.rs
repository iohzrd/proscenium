use super::client::{SyncSource, sync_from_peer};
use crate::constants::{
    MAX_MIRRORS_TRIED, MAX_NODE_IDS_TRIED, MIRROR_SYNC_TIMEOUT, SYNC_PEER_TIMEOUT,
};
use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::{Endpoint, EndpointId};
use proscenium_types::{Post, Visibility, is_valid_pubkey, short_id};
use tauri::AppHandle;
use tokio::time::Instant;

/// Stats returned from `sync_one_peer`.
pub struct SyncOneResult {
    /// Number of new post rows inserted.
    pub stored: usize,
    /// The newest newly stored posts (at most `SYNC_RETURNED_POSTS_MAX`, newest first).
    pub posts: Vec<Post>,
    /// Number of posts the author's device reported; `None` after a sync from a mirror.
    pub remote_post_count: Option<u64>,
}

/// Sync the content of the user `pubkey` from one of its cached transport NodeIds, or
/// else from one of `mirrors`.
///
/// Connects to at most `MAX_NODE_IDS_TRIED` NodeIds of the user in order. If none
/// answers and the stored profile of `pubkey` is Public, connects to at most
/// `MAX_MIRRORS_TRIED` of `mirrors` that are neither in the user's device list nor the
/// NodeId of a stored discovery server (which accepts only gossip): nodes of other users
/// that follow `pubkey` and serve its posts and post deletions, each for at most
/// `MIRROR_SYNC_TIMEOUT`. Runs for at most `SYNC_PEER_TIMEOUT` in total (plus one frame
/// timeout). Returns the result of the first node that answered with a usable summary.
/// Nothing is requested for a muted or blocked user.
pub async fn sync_one_peer(
    endpoint: &Endpoint,
    storage: &Storage,
    identity: &SharedIdentity,
    pubkey: &str,
    app_handle: &AppHandle,
    label: &str,
    mirrors: &[EndpointId],
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
            remote_post_count: None,
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
            endpoint,
            storage,
            identity,
            target,
            pubkey,
            app_handle,
            label,
            deadline,
            SyncSource::Author,
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

    let public = storage
        .get_profile(pubkey)
        .await?
        .is_some_and(|p| p.visibility == Visibility::Public);
    if public {
        let server_node_ids: Vec<String> = storage
            .get_servers()
            .await?
            .into_iter()
            .map(|server| server.node_id)
            .collect();
        let candidates = mirrors
            .iter()
            .filter(|m| {
                let id = m.to_string();
                !node_ids.contains(&id) && !server_node_ids.contains(&id)
            })
            .take(MAX_MIRRORS_TRIED);
        for &mirror in candidates {
            if Instant::now() >= deadline {
                last_err = "time limit reached".to_string();
                break;
            }
            let mirror_deadline = deadline.min(Instant::now() + MIRROR_SYNC_TIMEOUT);
            tried += 1;
            match sync_from_peer(
                endpoint,
                storage,
                identity,
                mirror,
                pubkey,
                app_handle,
                label,
                mirror_deadline,
                SyncSource::Mirror,
            )
            .await
            {
                Ok(result) => return Ok(result),
                Err(e) => {
                    log::debug!(
                        "[{label}] sync via mirror {} failed: {e}",
                        mirror.fmt_short()
                    );
                    last_err = e.to_string();
                }
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
        SyncSource::OwnDevice,
    )
    .await
}
