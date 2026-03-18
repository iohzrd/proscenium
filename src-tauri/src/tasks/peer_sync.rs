use crate::constants::{
    DRIP_ACTIVE_INTERVAL, DRIP_IDLE_INTERVAL, DRIP_PEER_PACE, PEER_READY_DELAY,
    RELAY_CHECK_INTERVAL, RELAY_WAIT_ATTEMPTS, SYNC_CONCURRENCY,
};
use crate::state::{AppState, SyncCommand};
use proscenium_types::short_id;
use std::sync::Arc;
use tokio::sync::mpsc;

pub fn spawn(state: Arc<AppState>, sync_rx: mpsc::Receiver<SyncCommand>) {
    let token = state.shutdown.child_token();
    tokio::spawn(async move {
        tokio::select! {
            _ = token.cancelled() => {}
            _ = run(state, sync_rx) => {}
        }
        log::info!("[peer-sync] task stopped");
    });
}

async fn run(state: Arc<AppState>, mut sync_rx: mpsc::Receiver<SyncCommand>) {
    // Wait until the relay is connected before syncing so we can actually reach peers.
    wait_for_relay(&state).await;

    // Small delay to let the network settle.
    tokio::time::sleep(PEER_READY_DELAY).await;

    // Startup: sync all follows in parallel batches.
    startup_sync(state.clone()).await;

    // Drip loop: slowly re-sync follows + handle on-demand SyncCommand requests.
    let mut drip_interval = tokio::time::interval(DRIP_IDLE_INTERVAL);
    drip_interval.tick().await; // consume the immediate first tick

    let mut follows_iter: Vec<String> = Vec::new();
    let mut follow_idx: usize = 0;
    let mut last_activity = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => break,

            cmd = sync_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    SyncCommand::SyncPeer(pubkey) => {
                        sync_one(state.clone(), &pubkey, "on-demand").await;
                        last_activity = std::time::Instant::now();
                    }
                    SyncCommand::SyncAll => {
                        startup_sync(state.clone()).await;
                        last_activity = std::time::Instant::now();
                    }
                }
                // Switch to active interval after any on-demand request.
                drip_interval = tokio::time::interval(DRIP_ACTIVE_INTERVAL);
                drip_interval.tick().await;
            }

            _ = drip_interval.tick() => {
                // Refresh the follows list when exhausted.
                if follow_idx >= follows_iter.len() {
                    follows_iter = get_follows(&state).await;
                    follow_idx = 0;
                }
                if follows_iter.is_empty() {
                    continue;
                }

                sync_one(state.clone(), &follows_iter[follow_idx], "drip").await;
                follow_idx += 1;

                // Pace between peers.
                tokio::time::sleep(DRIP_PEER_PACE).await;

                // Revert to idle interval if nothing has happened for a while.
                if last_activity.elapsed() > DRIP_ACTIVE_INTERVAL * 3 {
                    drip_interval = tokio::time::interval(DRIP_IDLE_INTERVAL);
                    drip_interval.tick().await;
                }
            }
        }
    }
}

async fn wait_for_relay(state: &AppState) {
    for attempt in 0..RELAY_WAIT_ATTEMPTS {
        if state.endpoint.addr().relay_urls().next().is_some() {
            log::info!("[peer-sync] relay connected (attempt {attempt})");
            return;
        }
        tokio::time::sleep(RELAY_CHECK_INTERVAL).await;
    }
    log::warn!(
        "[peer-sync] relay not connected after {RELAY_WAIT_ATTEMPTS} attempts, proceeding anyway"
    );
}

async fn startup_sync(state: Arc<AppState>) {
    let follows = get_follows(&state).await;
    if follows.is_empty() {
        return;
    }
    log::info!("[peer-sync] startup sync: {} peers", follows.len());

    for chunk in follows.chunks(SYNC_CONCURRENCY) {
        let mut handles = Vec::new();
        for pubkey in chunk {
            let state = state.clone();
            let pubkey = pubkey.clone();
            handles.push(tokio::spawn(async move {
                sync_one(state, &pubkey, "startup").await;
            }));
        }
        for h in handles {
            let _ = h.await;
        }
    }
}

async fn sync_one(state: Arc<AppState>, pubkey: &str, label: &str) {
    let my_id = state.identity.read().await.master_pubkey.clone();
    match crate::sync::sync_one_peer(
        &state.endpoint,
        &state.storage,
        pubkey,
        &my_id,
        &state.app_handle,
        label,
    )
    .await
    {
        Ok(result) => {
            log::info!(
                "[peer-sync] {label} {}: stored={} remote={}",
                short_id(pubkey),
                result.stored,
                result.remote_post_count,
            );
        }
        Err(e) => {
            log::debug!("[peer-sync] {label} {} offline: {e}", short_id(pubkey));
        }
    }
}

async fn get_follows(state: &AppState) -> Vec<String> {
    match state.storage.get_follows().await {
        Ok(entries) => entries.into_iter().map(|e| e.pubkey).collect(),
        Err(e) => {
            log::error!("[peer-sync] failed to load follows: {e}");
            Vec::new()
        }
    }
}
