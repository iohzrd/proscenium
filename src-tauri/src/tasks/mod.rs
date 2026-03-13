mod device_sync;
mod peer_sync;

use crate::state::{AppState, SyncCommand};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Spawn all application-level background tasks.
/// Services (GossipService, DmHandler) manage their own internal tasks via `start_background()`.
/// This function handles timer/external-command-driven cross-cutting tasks.
pub fn spawn_all(state: Arc<AppState>, sync_rx: mpsc::Receiver<SyncCommand>) {
    peer_sync::spawn(state.clone(), sync_rx);
    device_sync::spawn(state);
}
