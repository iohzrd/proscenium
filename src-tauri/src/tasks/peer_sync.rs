use crate::constants::{
    DRIP_ACTIVE_INTERVAL, DRIP_IDLE_INTERVAL, PEER_READY_DELAY, RELAY_CHECK_INTERVAL,
    RELAY_WAIT_ATTEMPTS, SYNC_CONCURRENCY,
};
use crate::state::{AppState, SyncCommand};
use proscenium_types::short_id;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::{Id, JoinError, JoinSet};

pub fn spawn(state: Arc<AppState>, sync_rx: mpsc::Receiver<SyncCommand>) {
    let token = state.shutdown().child_token();
    tokio::spawn(async move {
        tokio::select! {
            _ = token.cancelled() => {}
            _ = run(state, sync_rx) => {}
        }
        log::info!("[peer-sync] task stopped");
    });
}

/// Runs queued syncs, at most `SYNC_CONCURRENCY` at a time and one per user.
///
/// Dropping the scheduler aborts the running syncs (`JoinSet` aborts its tasks on drop).
struct SyncScheduler {
    state: Arc<AppState>,
    running: JoinSet<()>,
    /// Pubkey of each running task.
    task_pubkeys: HashMap<Id, String>,
    /// Pubkeys that are queued or running; a pubkey is not queued a second time.
    active: HashSet<String>,
    queue: VecDeque<(String, &'static str)>,
}

impl SyncScheduler {
    fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            running: JoinSet::new(),
            task_pubkeys: HashMap::new(),
            active: HashSet::new(),
            queue: VecDeque::new(),
        }
    }

    /// Queue a sync of `pubkey` unless one is queued or running.
    fn enqueue(&mut self, pubkey: String, label: &'static str) {
        if self.active.insert(pubkey.clone()) {
            self.queue.push_back((pubkey, label));
        }
    }

    /// Queue a sync of every followed user.
    async fn enqueue_follows(&mut self, label: &'static str) {
        let follows = get_follows(&self.state).await;
        if !follows.is_empty() {
            log::info!("[peer-sync] {label} sync: {} peers", follows.len());
        }
        for pubkey in follows {
            self.enqueue(pubkey, label);
        }
    }

    /// Start queued syncs while fewer than `SYNC_CONCURRENCY` are running.
    fn start_queued(&mut self) {
        while self.running.len() < SYNC_CONCURRENCY {
            let Some((pubkey, label)) = self.queue.pop_front() else {
                break;
            };
            let state = self.state.clone();
            let task_pubkey = pubkey.clone();
            let handle = self.running.spawn(async move {
                sync_one(state, &task_pubkey, label).await;
            });
            self.task_pubkeys.insert(handle.id(), pubkey);
        }
    }

    /// Record that a sync task ended, so its pubkey can be queued again.
    fn finished(&mut self, result: Result<(Id, ()), JoinError>) {
        let id = match result {
            Ok((id, ())) => id,
            Err(e) => {
                log::error!("[peer-sync] sync task failed: {e}");
                e.id()
            }
        };
        if let Some(pubkey) = self.task_pubkeys.remove(&id) {
            self.active.remove(&pubkey);
        }
    }
}

async fn run(state: Arc<AppState>, mut sync_rx: mpsc::Receiver<SyncCommand>) {
    // Wait until the relay is connected before syncing so we can actually reach peers.
    wait_for_relay(&state).await;

    // Small delay to let the network settle.
    tokio::time::sleep(PEER_READY_DELAY).await;

    // Startup: sync all follows. Then each drip tick syncs one follow in turn, and
    // commands on `sync_rx` queue syncs on demand. Syncs run in their own tasks, so a
    // slow peer does not delay the commands or the other peers.
    let mut scheduler = SyncScheduler::new(state.clone());
    scheduler.enqueue_follows("startup").await;
    scheduler.start_queued();

    let mut drip_interval = tokio::time::interval(DRIP_IDLE_INTERVAL);
    drip_interval.tick().await; // consume the immediate first tick

    let mut follows_iter: Vec<String> = Vec::new();
    let mut follow_idx: usize = 0;
    let mut last_activity = std::time::Instant::now();
    let mut active_interval = false;

    loop {
        let shutdown = state.shutdown();
        tokio::select! {
            _ = shutdown.cancelled() => break,

            cmd = sync_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    SyncCommand::SyncPeer(pubkey) => scheduler.enqueue(pubkey, "on-demand"),
                    SyncCommand::SyncAll => scheduler.enqueue_follows("on-demand").await,
                }
                last_activity = std::time::Instant::now();
                // Switch to the active interval after an on-demand request.
                if !active_interval {
                    active_interval = true;
                    drip_interval = tokio::time::interval(DRIP_ACTIVE_INTERVAL);
                    drip_interval.tick().await;
                }
            }

            Some(result) = scheduler.running.join_next_with_id(), if !scheduler.running.is_empty() => {
                scheduler.finished(result);
            }

            _ = drip_interval.tick() => {
                // Refresh the follows list when exhausted.
                if follow_idx >= follows_iter.len() {
                    follows_iter = get_follows(&state).await;
                    follow_idx = 0;
                }
                if let Some(pubkey) = follows_iter.get(follow_idx) {
                    scheduler.enqueue(pubkey.clone(), "drip");
                    follow_idx += 1;
                }

                // Revert to the idle interval if nothing has happened for a while.
                if active_interval && last_activity.elapsed() > DRIP_ACTIVE_INTERVAL * 3 {
                    active_interval = false;
                    drip_interval = tokio::time::interval(DRIP_IDLE_INTERVAL);
                    drip_interval.tick().await;
                }
            }
        }
        scheduler.start_queued();
    }
}

async fn wait_for_relay(state: &AppState) {
    for attempt in 0..RELAY_WAIT_ATTEMPTS {
        if state.endpoint().addr().relay_urls().next().is_some() {
            log::info!("[peer-sync] relay connected (attempt {attempt})");
            return;
        }
        tokio::time::sleep(RELAY_CHECK_INTERVAL).await;
    }
    log::warn!(
        "[peer-sync] relay not connected after {RELAY_WAIT_ATTEMPTS} attempts, proceeding anyway"
    );
}

async fn sync_one(state: Arc<AppState>, pubkey: &str, label: &str) {
    let mirrors = state.gossip().feed_neighbors(pubkey);
    match crate::sync::sync_one_peer(
        &state.endpoint(),
        &state.storage,
        &state.identity,
        pubkey,
        &state.app_handle,
        label,
        &mirrors,
    )
    .await
    {
        Ok(result) => {
            log::info!(
                "[peer-sync] {label} {}: stored={} remote={:?}",
                short_id(pubkey),
                result.stored,
                result.remote_post_count,
            );
        }
        Err(e) => {
            log::debug!("[peer-sync] {label} {} not synced: {e}", short_id(pubkey));
        }
    }
}

async fn get_follows(state: &AppState) -> Vec<String> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    match state.storage.get_follows(&my_id).await {
        Ok(entries) => entries.into_iter().map(|e| e.pubkey).collect(),
        Err(e) => {
            log::error!("[peer-sync] failed to load follows: {e}");
            Vec::new()
        }
    }
}
