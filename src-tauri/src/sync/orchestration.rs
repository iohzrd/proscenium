use super::client::sync_from_peer;
use super::processing::process_sync_result;
use crate::constants::SYNC_TIMEOUT;
use crate::error::AppError;
use crate::storage::Storage;
use iroh::{Endpoint, EndpointId};
use proscenium_types::{Post, short_id};
use tauri::{AppHandle, Emitter};

/// Stats returned from `sync_one_peer`.
pub struct SyncOneResult {
    pub stored: usize,
    pub posts: Vec<Post>,
    pub remote_post_count: u64,
}

/// Sync from a single peer: resolve transport NodeIds, try each, run sync protocol,
/// process and store results. Returns `SyncOneResult` on first successful NodeId.
pub async fn sync_one_peer(
    endpoint: &Endpoint,
    storage: &Storage,
    pubkey: &str,
    my_id: &str,
    app_handle: &AppHandle,
    label: &str,
) -> Result<SyncOneResult, AppError> {
    let node_ids = storage.get_peer_transport_node_ids(pubkey).await?;
    if node_ids.is_empty() {
        return Err(AppError::Other(format!(
            "no cached transport NodeId for {}",
            short_id(pubkey)
        )));
    }

    let mut last_err = String::new();
    for node_id in &node_ids {
        let target: EndpointId = match node_id.parse() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("[{label}] bad transport NodeId {}: {e}", short_id(node_id));
                continue;
            }
        };

        let result = tokio::time::timeout(
            SYNC_TIMEOUT,
            sync_from_peer(endpoint, storage, target, pubkey),
        )
        .await;

        match result {
            Ok(Ok(sync_result)) => {
                let stored =
                    process_sync_result(storage, pubkey, &sync_result, label, my_id, app_handle)
                        .await;

                // Emit stage announcement if the peer is hosting a live stage.
                if let Some(stage) = &sync_result.active_stage {
                    log::info!(
                        "[{label}] discovered live stage \"{}\" from {}",
                        stage.title,
                        short_id(pubkey),
                    );
                    let _ = app_handle.emit(
                        "stage-announced",
                        serde_json::json!({
                            "stage_id": stage.stage_id,
                            "title": stage.title,
                            "ticket": stage.ticket.to_string(),
                            "host_pubkey": stage.host_pubkey,
                            "started_at": stage.started_at,
                        }),
                    );
                }

                log::info!(
                    "[{label}] stored {stored}/{} posts from {} via {} (mode={:?})",
                    sync_result.posts.len(),
                    short_id(pubkey),
                    short_id(node_id),
                    sync_result.mode,
                );
                return Ok(SyncOneResult {
                    stored,
                    posts: sync_result.posts,
                    remote_post_count: sync_result.remote_post_count,
                });
            }
            Ok(Err(e)) => {
                log::warn!("[{label}] failed via {}: {e}", short_id(node_id));
                last_err = e.to_string();
            }
            Err(_) => {
                log::warn!("[{label}] timed out via {}", short_id(node_id));
                last_err = "timeout".to_string();
            }
        }
    }

    Err(AppError::Other(format!(
        "sync failed for {} (tried {} node(s)): {last_err}",
        short_id(pubkey),
        node_ids.len(),
    )))
}
