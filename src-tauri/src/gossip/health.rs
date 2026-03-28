use crate::constants::{GOSSIP_HEARTBEAT_INTERVAL, HEALTH_TICK_INTERVAL, WAKE_THRESHOLD};
use proscenium_types::short_id;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

impl super::GossipService {
    pub(super) async fn reconnect_loop(
        &self,
        mut rx: mpsc::UnboundedReceiver<String>,
        token: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                Some(pubkey) = rx.recv() => {
                    // Brief backoff before reconnecting.
                    tokio::select! {
                        _ = token.cancelled() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                    }
                    let node_ids = self
                        .storage
                        .get_peer_transport_node_ids(&pubkey)
                        .await
                        .unwrap_or_default();
                    if node_ids.is_empty() {
                        log::warn!(
                            "[gossip-reconnect] no NodeIds for {}, skipping",
                            short_id(&pubkey)
                        );
                        continue;
                    }
                    log::info!("[gossip-reconnect] reconnecting to {}", short_id(&pubkey));
                    if let Err(e) = self.follow_user(pubkey.clone(), node_ids).await {
                        log::error!(
                            "[gossip-reconnect] failed to reconnect to {}: {e}",
                            short_id(&pubkey)
                        );
                    }
                }
            }
        }
    }

    pub(super) async fn network_health_task(&self, token: CancellationToken) {
        let mut last_tick = std::time::Instant::now();
        let mut last_heartbeat = std::time::Instant::now();
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(HEALTH_TICK_INTERVAL) => {}
            }
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(last_tick);
            last_tick = now;

            if elapsed > WAKE_THRESHOLD {
                log::info!(
                    "[wake] sleep/wake detected ({:.0}s elapsed), refreshing network",
                    elapsed.as_secs_f64()
                );
                self.endpoint.network_change().await;
                self.refresh_all().await;
            }

            if last_heartbeat.elapsed() >= GOSSIP_HEARTBEAT_INTERVAL {
                last_heartbeat = std::time::Instant::now();
                if let Err(e) = self.broadcast_heartbeat().await {
                    log::error!("[heartbeat] broadcast failed: {e}");
                }
            }
        }
    }

    /// Re-queue reconnect for all subscriptions whose tasks have exited.
    pub async fn refresh_all(&self) {
        let inner = self.inner.lock().await;
        for (pubkey, (handle, _)) in &inner.subscriptions {
            if handle.is_finished() {
                let _ = self.reconnect_tx.send(pubkey.clone());
            }
        }
    }
}
