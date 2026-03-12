use crate::constants::OUTBOX_FLUSH_INTERVAL;
use crate::dm::DmHandler;
use crate::storage::Storage;
use iroh::Endpoint;
use iroh_social_types::short_id;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub async fn dm_outbox_flush_task(
    dm_handler: DmHandler,
    endpoint: Endpoint,
    storage: Arc<Storage>,
    outbox_notify: Arc<tokio::sync::Notify>,
    token: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = outbox_notify.notified() => {}
            _ = tokio::time::sleep(OUTBOX_FLUSH_INTERVAL) => {}
        }
        let peers = match storage.get_all_outbox_peers().await {
            Ok(p) => p,
            Err(e) => {
                log::error!("[dm-outbox] failed to get peers: {e}");
                continue;
            }
        };
        for peer in peers {
            match dm_handler.flush_outbox_for_peer(&endpoint, &peer).await {
                Ok((sent, _)) if sent > 0 => {
                    log::info!(
                        "[dm-outbox] flushed {sent} queued messages to {}",
                        short_id(&peer)
                    );
                }
                Err(e) => {
                    log::error!("[dm-outbox] flush error for {}: {e}", short_id(&peer));
                }
                _ => {}
            }
        }
    }
}
