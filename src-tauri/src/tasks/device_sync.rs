use crate::constants::{DEVICE_SYNC_INITIAL_DELAY, DEVICE_SYNC_INTERVAL};
use crate::storage::Storage;
use iroh::Endpoint;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub async fn device_sync_task(
    endpoint: Endpoint,
    storage: Arc<Storage>,
    master_pubkey: String,
    signing_secret_key_bytes: [u8; 32],
    token: CancellationToken,
) {
    tokio::select! {
        _ = token.cancelled() => return,
        _ = tokio::time::sleep(DEVICE_SYNC_INITIAL_DELAY) => {}
    }
    loop {
        crate::device_sync::sync_all_devices(
            &endpoint,
            &storage,
            &master_pubkey,
            &signing_secret_key_bytes,
        )
        .await;
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(DEVICE_SYNC_INTERVAL) => {}
        }
    }
}
