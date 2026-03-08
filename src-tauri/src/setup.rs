use crate::commands::sync::process_sync_result;
use crate::constants::*;
use crate::dm::DmHandler;
use crate::gossip::FeedManager;
use crate::peer::PeerHandler;
use crate::push;
use crate::state::AppState;
use crate::storage::Storage;
use crate::sync;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use iroh_gossip::Gossip;
use iroh_social_types::{DM_ALPN, PEER_ALPN, short_id};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

fn load_or_create_key(path: &std::path::Path) -> SecretKey {
    if path.exists() {
        let bytes = std::fs::read(path).expect("failed to read identity key");
        let bytes: [u8; 32] = bytes.try_into().expect("invalid key length");
        SecretKey::from_bytes(&bytes)
    } else {
        let mut key_bytes = [0u8; 32];
        getrandom::fill(&mut key_bytes).expect("failed to generate random key");
        let key = SecretKey::from_bytes(&key_bytes);
        std::fs::write(path, key.to_bytes()).expect("failed to write identity key");
        key
    }
}

async fn sync_peer_posts(
    endpoint: &Endpoint,
    storage: &Arc<Storage>,
    pubkey: &str,
    my_id: &str,
    handle: &AppHandle,
) {
    let target: iroh::EndpointId = match pubkey.parse() {
        Ok(t) => t,
        Err(_) => return,
    };

    for attempt in 1..=SYNC_MAX_RETRIES {
        log::info!(
            "[startup-sync] syncing from {} (attempt {}/{})...",
            short_id(pubkey),
            attempt,
            SYNC_MAX_RETRIES,
        );
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            SYNC_TIMEOUT,
            sync::sync_from_peer(endpoint, storage, target, pubkey),
        )
        .await;
        let elapsed = start.elapsed();

        match result {
            Ok(Ok(sync_result)) => {
                let stored = process_sync_result(
                    storage,
                    pubkey,
                    &sync_result,
                    "startup-sync",
                    my_id,
                    handle,
                );

                if stored > 0 || sync_result.profile.is_some() {
                    let _ = handle.emit("feed-updated", ());
                }
                log::info!(
                    "[startup-sync] stored {stored}/{} posts from {} in {:.1}s (mode={:?})",
                    sync_result.posts.len(),
                    short_id(pubkey),
                    elapsed.as_secs_f64(),
                    sync_result.mode,
                );
                return;
            }
            Ok(Err(e)) => {
                log::error!(
                    "[startup-sync] attempt {attempt} failed for {} after {:.1}s: {e:?}",
                    short_id(pubkey),
                    elapsed.as_secs_f64()
                );
            }
            Err(_) => {
                log::error!(
                    "[startup-sync] attempt {attempt} timed out for {} after {:.1}s",
                    short_id(pubkey),
                    elapsed.as_secs_f64()
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

pub fn initialize(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        use tauri_plugin_deep_link::DeepLinkExt;
        if let Err(e) = app.deep_link().register_all() {
            log::error!("[setup] failed to register deep link schemes: {e}");
        }
    }

    let handle = app.handle().clone();

    let data_dir = handle
        .path()
        .app_data_dir()
        .expect("failed to resolve app data dir");
    std::fs::create_dir_all(&data_dir).expect("failed to create app data dir");
    log::info!("[setup] data dir: {}", data_dir.display());

    let secret_key = load_or_create_key(&data_dir.join("identity.key"));
    let db_path = data_dir.join("social.db");
    let storage = Arc::new(Storage::open(&db_path).expect("failed to open database"));
    log::info!("[setup] database opened");

    let follows = storage.get_follows().unwrap_or_default();
    log::info!("[setup] loaded {} follows", follows.len());

    let secret_key_bytes = secret_key.to_bytes();
    let storage_clone = storage.clone();
    tauri::async_runtime::spawn(async move {
        log::info!("[setup] binding iroh endpoint...");
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![
                iroh_blobs::ALPN.to_vec(),
                iroh_gossip::ALPN.to_vec(),
                PEER_ALPN.to_vec(),
                DM_ALPN.to_vec(),
            ])
            .bind()
            .await
            .expect("failed to bind iroh endpoint");

        log::info!("[setup] Node ID: {}", endpoint.id());
        log::info!("[setup] addr (immediate): {:?}", endpoint.addr());

        let ep_clone = endpoint.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RELAY_LOG_DELAY).await;
            log::info!("[setup] addr (after 3s): {:?}", ep_clone.addr());
        });

        #[cfg(target_os = "android")]
        {
            let ep_net = endpoint.clone();
            tokio::spawn(async move {
                ep_net.network_change().await;
                log::info!("[android-net] initial network_change() sent");
                loop {
                    tokio::time::sleep(ANDROID_NET_INTERVAL).await;
                    ep_net.network_change().await;
                }
            });
        }

        let blobs_dir = data_dir.join("blobs");
        let store = FsStore::load(&blobs_dir)
            .await
            .expect("failed to open blob store");
        log::info!("[setup] blob store opened at {}", blobs_dir.display());

        let blobs = BlobsProtocol::new(&store, None);
        let gossip = Gossip::builder().spawn(endpoint.clone());
        log::info!("[setup] gossip started");

        let node_id_str = endpoint.id().to_string();
        let dm_handler = DmHandler::new(
            storage_clone.clone(),
            handle.clone(),
            secret_key_bytes,
            endpoint.id().to_string(),
        );
        let peer_handler =
            PeerHandler::new(storage_clone.clone(), node_id_str.clone(), handle.clone());

        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs.clone())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(PEER_ALPN, peer_handler)
            .accept(DM_ALPN, dm_handler.clone())
            .spawn();
        log::info!("[setup] router spawned");

        let (reconnect_tx, reconnect_rx) = tokio::sync::mpsc::unbounded_channel();
        let reconnect_tx_loop = reconnect_tx.clone();
        let mut feed = FeedManager::new(
            gossip,
            endpoint.clone(),
            storage_clone.clone(),
            handle.clone(),
            reconnect_tx,
        );

        if let Err(e) = feed.start_own_feed().await {
            log::error!("[setup] failed to start own feed: {e}");
        } else {
            log::info!("[setup] own gossip feed started");
        }

        if let Ok(Some(profile)) = storage_clone.get_profile(&node_id_str) {
            if let Err(e) = feed.broadcast_profile(&profile).await {
                log::error!("[setup] failed to broadcast profile: {e}");
            } else {
                log::info!("[setup] broadcast profile: {}", profile.display_name);
            }
        }

        for f in &follows {
            log::info!("[setup] resubscribing to {}...", short_id(&f.pubkey));
            if let Err(e) = feed.follow_user(f.pubkey.clone()).await {
                log::error!(
                    "[setup] failed to resubscribe to {}: {e}",
                    short_id(&f.pubkey)
                );
            } else {
                log::info!("[setup] resubscribed to {}", short_id(&f.pubkey));
            }
        }

        // Concurrent startup sync with semaphore for bounded parallelism
        let sync_endpoint = endpoint.clone();
        let sync_storage = storage_clone.clone();
        let sync_follows = follows.clone();
        let sync_handle = handle.clone();
        let sync_my_id = endpoint.id().to_string();
        tokio::spawn(async move {
            log::info!("[startup-sync] waiting for relay connectivity...");
            let mut has_relay = false;
            for i in 0..RELAY_WAIT_ATTEMPTS {
                let addr = sync_endpoint.addr();
                if addr.relay_urls().next().is_some() {
                    log::info!("[startup-sync] relay connected after {}s", i);
                    has_relay = true;
                    break;
                }
                tokio::time::sleep(RELAY_CHECK_INTERVAL).await;
            }
            if !has_relay {
                log::error!("[startup-sync] no relay after 10s, attempting sync anyway");
            }

            log::info!("[startup-sync] waiting 5s for peers to be ready...");
            tokio::time::sleep(PEER_READY_DELAY).await;

            let semaphore = Arc::new(tokio::sync::Semaphore::new(SYNC_CONCURRENCY));
            let mut join_set = tokio::task::JoinSet::new();

            for f in sync_follows {
                let ep = sync_endpoint.clone();
                let st = sync_storage.clone();
                let hdl = sync_handle.clone();
                let sem = semaphore.clone();
                let mid = sync_my_id.clone();
                join_set.spawn(async move {
                    let _permit = sem.acquire().await;
                    sync_peer_posts(&ep, &st, &f.pubkey, &mid, &hdl).await;
                });
            }

            while let Some(result) = join_set.join_next().await {
                if let Err(e) = result {
                    log::error!("[startup-sync] task panicked: {e}");
                }
            }
            log::info!("[startup-sync] done");
        });

        // Background drip sync
        let drip_endpoint = endpoint.clone();
        let drip_storage = storage_clone.clone();
        let drip_handle = handle.clone();
        let drip_my_id = endpoint.id().to_string();
        tokio::spawn(async move {
            tokio::time::sleep(DRIP_INITIAL_DELAY).await;

            loop {
                let follows = drip_storage.get_follows().unwrap_or_default();
                let mut any_work = false;

                for f in &follows {
                    let target: iroh::EndpointId = match f.pubkey.parse() {
                        Ok(t) => t,
                        Err(_) => continue,
                    };

                    log::info!("[drip-sync] syncing {}", short_id(&f.pubkey));

                    let result = tokio::time::timeout(
                        SYNC_TIMEOUT,
                        sync::sync_from_peer(&drip_endpoint, &drip_storage, target, &f.pubkey),
                    )
                    .await;

                    match result {
                        Ok(Ok(sync_result)) => {
                            if sync_result.posts.is_empty() && sync_result.interactions.is_empty() {
                                log::info!("[drip-sync] {} up to date", short_id(&f.pubkey),);
                                continue;
                            }

                            let stored = process_sync_result(
                                &drip_storage,
                                &f.pubkey,
                                &sync_result,
                                "drip-sync",
                                &drip_my_id,
                                &drip_handle,
                            );

                            if stored > 0 {
                                any_work = true;
                                let _ = drip_handle.emit("feed-updated", ());
                            }

                            log::info!(
                                "[drip-sync] stored {stored}/{} posts from {} (mode={:?})",
                                sync_result.posts.len(),
                                short_id(&f.pubkey),
                                sync_result.mode,
                            );
                        }
                        Ok(Err(e)) => {
                            log::error!("[drip-sync] failed for {}: {e}", short_id(&f.pubkey));
                        }
                        Err(_) => {
                            log::error!("[drip-sync] timed out for {}", short_id(&f.pubkey));
                        }
                    }

                    tokio::time::sleep(DRIP_PEER_PACE).await;
                }

                let delay = if any_work {
                    DRIP_ACTIVE_INTERVAL
                } else {
                    DRIP_IDLE_INTERVAL
                };
                tokio::time::sleep(delay).await;
            }
        });

        // DM outbox flush task
        let outbox_dm = dm_handler.clone();
        let outbox_ep = endpoint.clone();
        let outbox_storage = storage_clone.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(OUTBOX_FLUSH_INTERVAL).await;
                let peers = match outbox_storage.get_all_outbox_peers() {
                    Ok(p) => p,
                    Err(e) => {
                        log::error!("[dm-outbox] failed to get peers: {e}");
                        continue;
                    }
                };
                for peer in peers {
                    match outbox_dm.flush_outbox_for_peer(&outbox_ep, &peer).await {
                        Ok((sent, _)) if sent > 0 => {
                            log::info!(
                                "[dm-outbox] flushed {sent} queued messages to {}",
                                short_id(&peer)
                            );
                        }
                        Err(e) => {
                            log::error!("[dm-outbox] flush error for {}: {e}", short_id(&peer));
                        }
                        _ => {}
                    }
                }
            }
        });

        // Push outbox flush task
        let push_ep = endpoint.clone();
        let push_storage = storage_clone.clone();
        let push_my_id = endpoint.id().to_string();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PUSH_OUTBOX_FLUSH_INTERVAL).await;
                let peers = match push_storage.get_push_outbox_peers() {
                    Ok(p) => p,
                    Err(e) => {
                        log::error!("[push-outbox] failed to get peers: {e}");
                        continue;
                    }
                };
                for peer in peers {
                    let target: iroh::EndpointId = match peer.parse() {
                        Ok(t) => t,
                        Err(_) => continue,
                    };

                    let profile_entries = push_storage
                        .get_pending_push_profile_ids(&peer)
                        .unwrap_or_default();
                    let post_entries = push_storage
                        .get_pending_push_post_ids(&peer)
                        .unwrap_or_default();
                    let interaction_entries = push_storage
                        .get_pending_push_interaction_ids(&peer)
                        .unwrap_or_default();

                    if profile_entries.is_empty()
                        && post_entries.is_empty()
                        && interaction_entries.is_empty()
                    {
                        continue;
                    }

                    let mut posts = Vec::new();
                    let mut post_outbox_ids = Vec::new();
                    for (outbox_id, post_id) in &post_entries {
                        if let Ok(Some(post)) = push_storage.get_post_by_id(post_id) {
                            posts.push(post);
                        }
                        post_outbox_ids.push(*outbox_id);
                    }

                    let mut interactions = Vec::new();
                    let mut interaction_outbox_ids = Vec::new();
                    for (outbox_id, interaction_id) in &interaction_entries {
                        if let Ok(Some(interaction)) =
                            push_storage.get_interaction_by_id(interaction_id)
                        {
                            interactions.push(interaction);
                        }
                        interaction_outbox_ids.push(*outbox_id);
                    }

                    // Include profile if there are profile-only entries
                    let profile = if !profile_entries.is_empty() {
                        push_storage.get_profile(&push_my_id).ok().flatten()
                    } else {
                        None
                    };

                    let msg = iroh_social_types::PushMessage {
                        author: push_my_id.clone(),
                        posts,
                        interactions,
                        profile,
                    };

                    let mut all_ids: Vec<i64> = post_outbox_ids
                        .iter()
                        .chain(interaction_outbox_ids.iter())
                        .copied()
                        .collect();
                    all_ids.extend_from_slice(&profile_entries);

                    match push::push_to_peer(&push_ep, target, &msg).await {
                        Ok(ack) => {
                            log::info!(
                                "[push-outbox] delivered {} posts, {} interactions to {}{}",
                                ack.received_post_ids.len(),
                                ack.received_interaction_ids.len(),
                                short_id(&peer),
                                if profile_entries.is_empty() {
                                    ""
                                } else {
                                    " (+ profile)"
                                },
                            );
                            let _ = push_storage.remove_push_outbox_entries(&all_ids);
                        }
                        Err(e) => {
                            log::error!("[push-outbox] failed to push to {}: {e}", short_id(&peer));
                            let _ = push_storage.mark_push_attempted(&all_ids);
                        }
                    }
                }
            }
        });

        // Push outbox prune task
        let prune_storage = storage_clone.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PUSH_OUTBOX_PRUNE_INTERVAL).await;
                match prune_storage.prune_expired_push_entries() {
                    Ok(count) if count > 0 => {
                        log::info!("[push-outbox] pruned {count} expired entries");
                    }
                    Err(e) => {
                        log::error!("[push-outbox] prune error: {e}");
                    }
                    _ => {}
                }
            }
        });

        // Follow request expiry task
        let follow_req_storage = storage_clone.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(FOLLOW_REQUEST_PRUNE_INTERVAL).await;
                match follow_req_storage.prune_expired_follow_requests() {
                    Ok(count) if count > 0 => {
                        log::info!("[follow-req] pruned {count} expired requests");
                    }
                    Err(e) => {
                        log::error!("[follow-req] prune error: {e}");
                    }
                    _ => {}
                }
            }
        });

        let state = Arc::new(AppState {
            endpoint,
            router,
            blobs,
            store,
            storage: storage_clone,
            feed: Arc::new(Mutex::new(feed)),
            dm: dm_handler,
            secret_key_bytes,
        });

        // Gossip reconnection loop: restarts dead gossip tasks on demand
        let reconnect_feed = state.feed.clone();
        tokio::spawn(async move {
            crate::gossip::gossip_reconnect_loop(reconnect_rx, reconnect_feed, reconnect_tx_loop)
                .await;
        });

        handle.manage(state);
        log::info!("[setup] app state ready");
    });

    Ok(())
}
