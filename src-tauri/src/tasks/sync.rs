use crate::constants::*;
use crate::gossip::GossipHandle;
use crate::storage::Storage;
use crate::sync;
use iroh::Endpoint;
use iroh_social_types::short_id;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

async fn sync_peer_posts(
    endpoint: &Endpoint,
    storage: &Arc<Storage>,
    pubkey: &str,
    my_id: &str,
    handle: &AppHandle,
) {
    for attempt in 1..=SYNC_MAX_RETRIES {
        log::info!(
            "[startup-sync] syncing from {} (attempt {}/{})...",
            short_id(pubkey),
            attempt,
            SYNC_MAX_RETRIES,
        );
        match sync::sync_one_peer(endpoint, storage, pubkey, my_id, handle, "startup-sync").await {
            Ok(result) => {
                if result.stored > 0 || result.profile.is_some() {
                    let _ = handle.emit("feed-updated", ());
                }
                return;
            }
            Err(e) => {
                log::error!(
                    "[startup-sync] attempt {attempt} failed for {}: {e}",
                    short_id(pubkey),
                );
            }
        }
        if attempt < SYNC_MAX_RETRIES {
            let delay = attempt as u64 * 5;
            log::info!(
                "[startup-sync] retrying {} in {delay}s...",
                short_id(pubkey)
            );
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        }
    }
}

/// Gossip follow subscriptions + startup sync + drip sync.
/// All in one task so drip naturally starts after startup-sync finishes,
/// avoiding concurrent syncs to the same peer.
#[allow(clippy::too_many_arguments)]
pub async fn subscribe_and_sync_task(
    gossip: GossipHandle,
    endpoint: Endpoint,
    storage: Arc<Storage>,
    app_handle: AppHandle,
    my_id: String,
    follows: Vec<iroh_social_types::FollowEntry>,
    mut sync_rx: tokio::sync::mpsc::Receiver<crate::state::SyncCommand>,
    token: CancellationToken,
) {
    // Wait for relay connectivity before subscribing to follows
    log::info!("[startup] waiting for relay connectivity...");
    let mut has_relay = false;
    for i in 0..RELAY_WAIT_ATTEMPTS {
        if token.is_cancelled() {
            log::info!("[startup] shutdown during relay wait");
            return;
        }
        let addr = endpoint.addr();
        if addr.relay_urls().next().is_some() {
            log::info!("[startup] relay connected after {}s", i);
            has_relay = true;
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(RELAY_CHECK_INTERVAL) => {}
            _ = token.cancelled() => {
                log::info!("[startup] shutdown during relay wait");
                return;
            }
        }
    }
    if !has_relay {
        log::error!("[startup] no relay after 10s, attempting subscriptions anyway");
    }

    // Subscribe to followed users' gossip topics
    for f in &follows {
        let node_ids = storage
            .get_peer_transport_node_ids(&f.pubkey)
            .await
            .unwrap_or_default();
        if node_ids.is_empty() {
            log::warn!(
                "[setup] no cached transport NodeIds for {}, skipping gossip subscribe",
                short_id(&f.pubkey)
            );
            continue;
        }
        log::info!("[setup] resubscribing to {}...", short_id(&f.pubkey));
        if let Err(e) = gossip.follow_user(f.pubkey.clone(), node_ids).await {
            log::error!(
                "[setup] failed to resubscribe to {}: {e}",
                short_id(&f.pubkey)
            );
        } else {
            log::info!("[setup] resubscribed to {}", short_id(&f.pubkey));
        }
    }

    // Startup sync: parallel fetch from all followed peers
    log::info!("[startup-sync] waiting 5s for peers to be ready...");
    tokio::select! {
        _ = tokio::time::sleep(PEER_READY_DELAY) => {}
        _ = token.cancelled() => {
            log::info!("[startup-sync] shutdown before sync");
            return;
        }
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(SYNC_CONCURRENCY));
    let mut join_set = tokio::task::JoinSet::new();

    for f in &follows {
        let ep = endpoint.clone();
        let st = storage.clone();
        let hdl = app_handle.clone();
        let sem = semaphore.clone();
        let mid = my_id.clone();
        let pk = f.pubkey.clone();
        join_set.spawn(async move {
            let _permit = sem.acquire().await;
            sync_peer_posts(&ep, &st, &pk, &mid, &hdl).await;
        });
    }

    while let Some(result) = join_set.join_next().await {
        if let Err(e) = result {
            log::error!("[startup-sync] task panicked: {e}");
        }
    }
    log::info!("[startup-sync] done, starting drip sync");

    // Drip sync: periodic sync with all followed peers, interruptible by SyncCommand.
    loop {
        if token.is_cancelled() {
            break;
        }

        // Wait for either a command or the idle interval, whichever comes first.
        let cmd = tokio::select! {
            _ = token.cancelled() => break,
            cmd = tokio::time::timeout(DRIP_IDLE_INTERVAL, sync_rx.recv()) => {
                match cmd {
                    Ok(Some(cmd)) => Some(cmd),
                    Ok(None) => break, // channel closed
                    Err(_) => None,    // timeout => periodic drip
                }
            }
        };

        let peers_to_sync: Vec<String> = match cmd {
            Some(crate::state::SyncCommand::SyncPeer(pubkey)) => vec![pubkey],
            Some(crate::state::SyncCommand::SyncAll) | None => storage
                .get_follows()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.pubkey)
                .collect(),
        };

        let mut any_work = false;
        for pubkey in &peers_to_sync {
            if token.is_cancelled() {
                break;
            }

            match sync::sync_one_peer(
                &endpoint,
                &storage,
                pubkey,
                &my_id,
                &app_handle,
                "drip-sync",
            )
            .await
            {
                Ok(result) if result.stored > 0 => {
                    any_work = true;
                    let _ = app_handle.emit("feed-updated", ());
                }
                Err(e) => {
                    log::error!("[drip-sync] failed for {}: {e}", short_id(pubkey));
                }
                _ => {}
            }

            tokio::select! {
                _ = tokio::time::sleep(DRIP_PEER_PACE) => {}
                _ = token.cancelled() => break,
            }
        }

        // If work was done, use a shorter interval before the next round
        if any_work && !token.is_cancelled() {
            tokio::select! {
                _ = tokio::time::sleep(DRIP_ACTIVE_INTERVAL) => {}
                _ = token.cancelled() => break,
            }
        }
    }
    log::info!("[drip-sync] task exited cleanly");
}
