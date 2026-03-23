use crate::constants::{DEVICE_SYNC_INITIAL_DELAY, DEVICE_SYNC_INTERVAL};
use crate::state::AppState;
use std::sync::Arc;

pub fn spawn(state: Arc<AppState>) {
    let token = state.shutdown().child_token();
    tokio::spawn(async move {
        tokio::select! {
            _ = token.cancelled() => {}
            _ = run(state) => {}
        }
        log::info!("[device-sync] task stopped");
    });
}

async fn run(state: Arc<AppState>) {
    // Wait before first sync so the endpoint has time to connect to the relay.
    tokio::time::sleep(DEVICE_SYNC_INITIAL_DELAY).await;

    loop {
        // Read identity live so key rotation is immediately reflected.
        let (master_pubkey, signing_secret_key_bytes) = {
            let id = state.identity.read().await;
            (id.master_pubkey.clone(), id.signing_secret_key_bytes)
        };

        crate::device_sync::sync_all_devices(
            &state.endpoint(),
            &state.storage,
            &master_pubkey,
            &signing_secret_key_bytes,
        )
        .await;

        let shutdown = state.shutdown();
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(DEVICE_SYNC_INTERVAL) => {}
        }
    }
}
