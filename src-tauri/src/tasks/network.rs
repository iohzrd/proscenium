use crate::constants::{GOSSIP_HEARTBEAT_INTERVAL, HEALTH_TICK_INTERVAL, WAKE_THRESHOLD};
use crate::gossip::GossipService;
use iroh::Endpoint;
use tokio_util::sync::CancellationToken;

pub async fn network_health_task(
    endpoint: Endpoint,
    gossip: GossipService,
    token: CancellationToken,
) {
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

        // Sleep/wake detection
        if elapsed > WAKE_THRESHOLD {
            log::info!(
                "[wake] detected sleep/wake (elapsed {:.0}s, expected {:.0}s), refreshing network",
                elapsed.as_secs_f64(),
                HEALTH_TICK_INTERVAL.as_secs_f64(),
            );
            endpoint.network_change().await;
            gossip.refresh_all();
        }

        // Heartbeat broadcast
        if last_heartbeat.elapsed() >= GOSSIP_HEARTBEAT_INTERVAL {
            last_heartbeat = std::time::Instant::now();
            if let Err(e) = gossip.broadcast_heartbeat().await {
                log::error!("[heartbeat] broadcast failed: {e}");
            }
        }
    }
}
