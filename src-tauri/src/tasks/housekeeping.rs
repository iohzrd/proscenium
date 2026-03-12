use crate::constants::HOUSEKEEPING_INTERVAL;
use crate::storage::Storage;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub async fn housekeeping_task(storage: Arc<Storage>, token: CancellationToken) {
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(HOUSEKEEPING_INTERVAL) => {}
        }
        match storage.prune_expired_push_entries().await {
            Ok(count) if count > 0 => {
                log::info!("[housekeeping] pruned {count} expired push entries");
            }
            Err(e) => {
                log::error!("[housekeeping] push prune error: {e}");
            }
            _ => {}
        }
        match storage.prune_expired_follow_requests().await {
            Ok(count) if count > 0 => {
                log::info!("[housekeeping] pruned {count} expired follow requests");
            }
            Err(e) => {
                log::error!("[housekeeping] follow request prune error: {e}");
            }
            _ => {}
        }
    }
}
