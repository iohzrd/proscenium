use crate::constants::{
    GOSSIP_RECONNECT_BASE_DELAY, GOSSIP_RECONNECT_MAX_DELAY, HEALTH_TICK_INTERVAL, WAKE_THRESHOLD,
};
use proscenium_types::short_id;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// A subscription waiting to be opened again.
struct PendingReconnect {
    /// Failed attempts so far.
    attempts: u32,
    /// Earliest time of the next attempt.
    due: Instant,
}

/// Delay before attempt number `attempts + 1`: the base delay doubled per failed attempt,
/// at most `GOSSIP_RECONNECT_MAX_DELAY`.
fn reconnect_delay(attempts: u32) -> Duration {
    GOSSIP_RECONNECT_BASE_DELAY
        .saturating_mul(1u32.checked_shl(attempts).unwrap_or(u32::MAX))
        .min(GOSSIP_RECONNECT_MAX_DELAY)
}

impl super::GossipService {
    /// Open subscriptions again after their task ended.
    ///
    /// Requests arrive on `rx`. Each pubkey is attempted after `reconnect_delay`; all
    /// pubkeys that are due are attempted in one pass, without a delay between them. A
    /// failed attempt is repeated with a doubled delay. A pubkey the local user no longer
    /// follows is discarded.
    pub(super) async fn reconnect_loop(
        &self,
        mut rx: mpsc::UnboundedReceiver<String>,
        token: CancellationToken,
    ) {
        let mut pending: HashMap<String, PendingReconnect> = HashMap::new();
        loop {
            let next_due = pending.values().map(|p| p.due).min();
            tokio::select! {
                _ = token.cancelled() => break,
                request = rx.recv() => {
                    let Some(pubkey) = request else { break };
                    pending.entry(pubkey).or_insert_with(|| PendingReconnect {
                        attempts: 0,
                        due: Instant::now() + reconnect_delay(0),
                    });
                }
                _ = async {
                    match next_due {
                        Some(due) => tokio::time::sleep_until(due).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.reconnect_due(&mut pending).await;
                }
            }
        }
    }

    /// Attempt every pending subscription whose time has come.
    async fn reconnect_due(&self, pending: &mut HashMap<String, PendingReconnect>) {
        let now = Instant::now();
        let due: Vec<String> = pending
            .iter()
            .filter(|(_, p)| p.due <= now)
            .map(|(pubkey, _)| pubkey.clone())
            .collect();
        let my_id = self.identity.read().await.master_pubkey.clone();
        for pubkey in due {
            let result = match self.storage.is_following(&my_id, &pubkey).await {
                Ok(true) => {
                    let node_ids = self
                        .storage
                        .get_peer_transport_node_ids(&pubkey)
                        .await
                        .unwrap_or_default();
                    if node_ids.is_empty() {
                        Err("no cached NodeIds".to_string())
                    } else {
                        self.follow_user(pubkey.clone(), node_ids)
                            .await
                            .map_err(|e| e.to_string())
                    }
                }
                // Unfollowed since the request was queued: do not subscribe again.
                Ok(false) => {
                    pending.remove(&pubkey);
                    let mut inner = self.inner.lock().await;
                    if inner
                        .subscriptions
                        .get(&pubkey)
                        .is_some_and(|handle| handle.is_finished())
                    {
                        inner.subscriptions.remove(&pubkey);
                    }
                    continue;
                }
                Err(e) => Err(format!("follow lookup failed: {e}")),
            };
            match result {
                Ok(()) => {
                    log::info!("[gossip-reconnect] subscribed to {}", short_id(&pubkey));
                    pending.remove(&pubkey);
                }
                Err(reason) => {
                    if let Some(entry) = pending.get_mut(&pubkey) {
                        entry.attempts = entry.attempts.saturating_add(1);
                        let delay = reconnect_delay(entry.attempts);
                        entry.due = Instant::now() + delay;
                        log::warn!(
                            "[gossip-reconnect] {} failed ({reason}), next attempt in {}s",
                            short_id(&pubkey),
                            delay.as_secs()
                        );
                    }
                }
            }
        }
    }

    /// Detect a suspend and resume of the process and refresh the network afterwards.
    ///
    /// Compares wall-clock time across ticks: the monotonic clock used by `tokio::time`
    /// and `std::time::Instant` does not advance while the system is suspended on Linux,
    /// Android and macOS, so a tick that takes much longer on the wall clock than
    /// `HEALTH_TICK_INTERVAL` indicates a suspend.
    pub(super) async fn network_health_task(&self, token: CancellationToken) {
        let mut last_tick = SystemTime::now();
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(HEALTH_TICK_INTERVAL) => {}
            }
            let now = SystemTime::now();
            // A wall clock that moved backwards yields an error; treated as no elapsed time.
            let elapsed = now.duration_since(last_tick).unwrap_or_default();
            last_tick = now;

            if elapsed > WAKE_THRESHOLD {
                log::info!(
                    "[wake] suspend detected ({:.0}s between ticks), refreshing network",
                    elapsed.as_secs_f64()
                );
                self.endpoint.network_change().await;
                self.refresh_all().await;
            }
        }
    }

    /// Queue a reconnect for every subscription whose task has ended.
    pub async fn refresh_all(&self) {
        let inner = self.inner.lock().await;
        for (pubkey, handle) in &inner.subscriptions {
            if handle.is_finished() {
                let _ = self.reconnect_tx.send(pubkey.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_delay_doubles_up_to_the_maximum() {
        assert_eq!(reconnect_delay(0), GOSSIP_RECONNECT_BASE_DELAY);
        assert_eq!(reconnect_delay(1), GOSSIP_RECONNECT_BASE_DELAY * 2);
        assert_eq!(reconnect_delay(3), GOSSIP_RECONNECT_BASE_DELAY * 8);
        assert_eq!(reconnect_delay(20), GOSSIP_RECONNECT_MAX_DELAY);
        assert_eq!(reconnect_delay(u32::MAX), GOSSIP_RECONNECT_MAX_DELAY);
    }
}
