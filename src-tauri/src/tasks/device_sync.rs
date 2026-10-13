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
        crate::device_sync::sync_all_devices(&state).await;
        tokio::time::sleep(DEVICE_SYNC_INTERVAL).await;
    }
}
