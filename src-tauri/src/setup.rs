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
use iroh_social_types::{
    DM_ALPN, DeviceEntry, LinkedDevicesAnnouncement, PEER_ALPN, derive_signing_key,
    derive_transport_key, now_millis, short_id, sign_delegation, sign_linked_devices_announcement,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

/// Load raw key bytes from a file, or generate new ones.
fn load_or_create_key_bytes(path: &std::path::Path) -> [u8; 32] {
    if path.exists() {
        let bytes = std::fs::read(path).expect("failed to read key file");
        bytes.try_into().expect("invalid key length")
    } else {
        let mut key_bytes = [0u8; 32];
        getrandom::fill(&mut key_bytes).expect("failed to generate random key");
        std::fs::write(path, key_bytes).expect("failed to write key file");
        key_bytes
    }
}

/// Load the persisted signing key index (returns 0 if not yet saved).
fn load_signing_key_index(data_dir: &std::path::Path) -> u32 {
    let path = data_dir.join("signing_key_index");
    match std::fs::read_to_string(&path) {
        Ok(s) => s.trim().parse().unwrap_or(0),
        Err(_) => 0,
    }
}

/// Save the signing key index to disk.
pub fn save_signing_key_index(data_dir: &std::path::Path, index: u32) {
    let path = data_dir.join("signing_key_index");
    std::fs::write(path, index.to_string()).expect("failed to write signing_key_index");
}

/// Migrate old identity.key to master_key.key if needed.
fn migrate_identity_key(data_dir: &std::path::Path) {
    let old_path = data_dir.join("identity.key");
    let new_path = data_dir.join("master_key.key");
    if old_path.exists() && !new_path.exists() {
        log::info!("[setup] migrating identity.key -> master_key.key");
        std::fs::rename(&old_path, &new_path).expect("failed to rename identity.key");
    }
}

async fn sync_peer_posts(
    endpoint: &Endpoint,
    storage: &Arc<Storage>,
    pubkey: &str,
    my_id: &str,
    handle: &AppHandle,
) {
    // Resolve transport NodeId from peer delegation cache
    let node_ids = storage
        .get_peer_transport_node_ids(pubkey)
        .unwrap_or_default();
    let target: iroh::EndpointId = if let Some(first) = node_ids.first() {
        match first.parse() {
            Ok(t) => t,
            Err(_) => return,
        }
    } else {
        log::warn!(
            "[startup-sync] no cached transport NodeIds for {}, skipping",
            short_id(pubkey)
        );
        return;
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

    // Migrate old identity.key -> master_key.key if needed
    migrate_identity_key(&data_dir);

    // Load or create master key (permanent identity)
    let master_secret_key_bytes = load_or_create_key_bytes(&data_dir.join("master_key.key"));
    let master_secret = SecretKey::from_bytes(&master_secret_key_bytes);
    let master_pubkey = master_secret.public().to_string();
    log::info!("[setup] master pubkey: {}", short_id(&master_pubkey));

    // Load persisted signing key index (defaults to 0 for fresh installs)
    let signing_key_index: u32 = load_signing_key_index(&data_dir);
    let signing_secret_key_bytes = derive_signing_key(&master_secret_key_bytes, signing_key_index);
    let signing_secret = SecretKey::from_bytes(&signing_secret_key_bytes);
    let signing_pubkey = signing_secret.public().to_string();
    log::info!("[setup] signing pubkey: {}", short_id(&signing_pubkey));

    // Sign delegation binding signing key to master key
    let delegation = sign_delegation(
        &master_secret,
        &signing_secret.public(),
        signing_key_index,
        now_millis(),
    );

    let db_path = data_dir.join("social.db");
    let storage = Arc::new(Storage::open(&db_path).expect("failed to open database"));
    log::info!("[setup] database opened");

    let follows = storage.get_follows().unwrap_or_default();
    log::info!("[setup] loaded {} follows", follows.len());

    // Derive stable transport key from master key (device index 0 = primary device)
    let device_index: u32 = 0;
    let transport_key_bytes = derive_transport_key(&master_secret_key_bytes, device_index);
    let transport_secret = SecretKey::from_bytes(&transport_key_bytes);

    let master_pubkey_clone = master_pubkey.clone();
    let signing_pubkey_clone = signing_pubkey.clone();
    let delegation_clone = delegation.clone();
    let storage_clone = storage.clone();
    tauri::async_runtime::spawn(async move {
        log::info!("[setup] binding iroh endpoint...");
        let endpoint = Endpoint::builder()
            .secret_key(transport_secret)
            .alpns(vec![
                iroh_blobs::ALPN.to_vec(),
                iroh_gossip::ALPN.to_vec(),
                PEER_ALPN.to_vec(),
                DM_ALPN.to_vec(),
            ])
            .bind()
            .await
            .expect("failed to bind iroh endpoint");

        let transport_node_id = endpoint.id().to_string();
        log::info!("[setup] Transport NodeId: {}", short_id(&transport_node_id));
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

        // DM handler uses signing key for X25519 derivation
        let dm_handler = DmHandler::new(
            storage_clone.clone(),
            handle.clone(),
            signing_secret_key_bytes,
            master_pubkey_clone.clone(),
        );
        // Shared pending link state for device pairing
        let pending_link: crate::state::PendingLinkState = Arc::new(tokio::sync::Mutex::new(None));

        // Peer handler needs master pubkey, delegation, and transport node id
        let peer_handler = PeerHandler::new(
            storage_clone.clone(),
            master_pubkey_clone.clone(),
            transport_node_id.clone(),
            delegation_clone.clone(),
            master_secret_key_bytes,
            signing_secret_key_bytes,
            pending_link.clone(),
            handle.clone(),
        );

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
            master_pubkey_clone.clone(),
            storage_clone.clone(),
            handle.clone(),
            reconnect_tx,
        );

        if let Err(e) = feed.start_own_feed().await {
            log::error!("[setup] failed to start own feed: {e}");
        } else {
            log::info!("[setup] own gossip feed started");
        }

        if let Ok(Some(profile)) = storage_clone.get_profile(&master_pubkey_clone) {
            if let Err(e) = feed.broadcast_profile(&profile).await {
                log::error!("[setup] failed to broadcast profile: {e}");
            } else {
                log::info!("[setup] broadcast profile: {}", profile.display_name);
            }
        }

        // Register this device and broadcast single-device announcement
        {
            let now = now_millis();
            if let Err(e) = storage_clone.upsert_linked_device(
                &transport_node_id,
                "Primary Device",
                true,
                true,
                now,
            ) {
                log::error!("[setup] failed to register own device: {e}");
            }

            let signing_sk = SecretKey::from_bytes(&signing_secret_key_bytes);
            let mut announcement = LinkedDevicesAnnouncement {
                master_pubkey: master_pubkey_clone.clone(),
                delegation: delegation_clone.clone(),
                devices: vec![DeviceEntry {
                    node_id: transport_node_id.clone(),
                    device_name: "Primary Device".to_string(),
                    is_primary: true,
                    added_at: now,
                }],
                version: 1,
                timestamp: now,
                signature: String::new(),
            };
            sign_linked_devices_announcement(&mut announcement, &signing_sk);

            if let Err(e) = feed.broadcast_linked_devices(&announcement).await {
                log::error!("[setup] failed to broadcast device announcement: {e}");
            } else {
                log::info!("[setup] broadcast single-device announcement");
            }
        }

        // Wrap feed in Arc<Mutex> early so the spawned subscription task can use it
        let shared_feed = Arc::new(Mutex::new(feed));

        // Gossip follow subscriptions + startup sync -- both need relay
        let sub_feed = shared_feed.clone();
        let sub_follows = follows.clone();
        let sub_storage = storage_clone.clone();
        let sync_endpoint = endpoint.clone();
        let sync_storage = storage_clone.clone();
        let sync_follows = follows.clone();
        let sync_handle = handle.clone();
        let sync_my_id = master_pubkey_clone.clone();
        tokio::spawn(async move {
            // Wait for relay connectivity before subscribing to follows
            log::info!("[startup] waiting for relay connectivity...");
            let mut has_relay = false;
            for i in 0..RELAY_WAIT_ATTEMPTS {
                let addr = sync_endpoint.addr();
                if addr.relay_urls().next().is_some() {
                    log::info!("[startup] relay connected after {}s", i);
                    has_relay = true;
                    break;
                }
                tokio::time::sleep(RELAY_CHECK_INTERVAL).await;
            }
            if !has_relay {
                log::error!("[startup] no relay after 10s, attempting subscriptions anyway");
            }

            // Subscribe to followed users' gossip topics
            for f in &sub_follows {
                let node_ids = sub_storage
                    .get_peer_transport_node_ids(&f.pubkey)
                    .unwrap_or_default();
                if node_ids.is_empty() {
                    log::warn!(
                        "[setup] no cached transport NodeIds for {}, skipping gossip subscribe",
                        short_id(&f.pubkey)
                    );
                    continue;
                }
                log::info!("[setup] resubscribing to {}...", short_id(&f.pubkey));
                let mut feed = sub_feed.lock().await;
                if let Err(e) = feed.follow_user(f.pubkey.clone(), &node_ids).await {
                    log::error!(
                        "[setup] failed to resubscribe to {}: {e}",
                        short_id(&f.pubkey)
                    );
                } else {
                    log::info!("[setup] resubscribed to {}", short_id(&f.pubkey));
                }
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
        let drip_my_id = master_pubkey_clone.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DRIP_INITIAL_DELAY).await;

            loop {
                let follows = drip_storage.get_follows().unwrap_or_default();
                let mut any_work = false;

                for f in &follows {
                    let node_ids = drip_storage
                        .get_peer_transport_node_ids(&f.pubkey)
                        .unwrap_or_default();
                    let target: iroh::EndpointId = if let Some(first) = node_ids.first() {
                        match first.parse() {
                            Ok(t) => t,
                            Err(_) => continue,
                        }
                    } else {
                        continue;
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
        let push_my_id = master_pubkey_clone.clone();
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
                    let peer_node_ids = push_storage
                        .get_peer_transport_node_ids(&peer)
                        .unwrap_or_default();
                    let targets: Vec<iroh::EndpointId> = peer_node_ids
                        .iter()
                        .filter_map(|id| id.parse().ok())
                        .collect();
                    if targets.is_empty() {
                        continue;
                    }

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

                    let mut delivered = false;
                    for target in &targets {
                        match push::push_to_peer(&push_ep, *target, &msg).await {
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
                                delivered = true;
                                break;
                            }
                            Err(e) => {
                                log::debug!(
                                    "[push-outbox] failed to push to {} device: {e}",
                                    short_id(&peer)
                                );
                            }
                        }
                    }
                    if delivered {
                        let _ = push_storage.remove_push_outbox_entries(&all_ids);
                    } else {
                        log::error!(
                            "[push-outbox] failed to push to {} (tried {} devices)",
                            short_id(&peer),
                            targets.len()
                        );
                        let _ = push_storage.mark_push_attempted(&all_ids);
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

        // Periodic device sync task
        let dsync_ep = endpoint.clone();
        let dsync_storage = storage_clone.clone();
        let dsync_master = master_pubkey_clone.clone();
        let dsync_signing = signing_secret_key_bytes;
        tokio::spawn(async move {
            tokio::time::sleep(DEVICE_SYNC_INITIAL_DELAY).await;
            loop {
                crate::device_sync::sync_all_devices(
                    &dsync_ep,
                    &dsync_storage,
                    &dsync_master,
                    &dsync_signing,
                )
                .await;
                tokio::time::sleep(DEVICE_SYNC_INTERVAL).await;
            }
        });

        let state = Arc::new(AppState {
            endpoint,
            router,
            blobs,
            store,
            storage: storage_clone,
            feed: shared_feed,
            dm: dm_handler,
            master_secret_key_bytes,
            master_pubkey: master_pubkey_clone.clone(),
            signing_secret_key_bytes,
            signing_pubkey: signing_pubkey_clone,
            signing_key_index,
            transport_node_id: transport_node_id.clone(),
            delegation: delegation_clone,
            pending_link,
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
