use crate::constants::*;
use crate::dm::DmHandler;
use crate::gossip::{GossipActor, GossipHandle};
use crate::peer::PeerHandler;
use crate::push;
use crate::state::AppState;
use crate::storage::Storage;
use crate::sync;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use iroh_gossip::Gossip;
use iroh_social_types::{
    DM_ALPN, DeviceEntry, LinkedDevicesAnnouncement, PEER_ALPN, derive_dm_key, derive_signing_key,
    derive_transport_key, now_millis, short_id, sign_delegation, sign_linked_devices_announcement,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

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

/// Load the persisted DM key index (returns 0 if not yet saved).
fn load_dm_key_index(data_dir: &std::path::Path) -> u32 {
    let path = data_dir.join("dm_key_index");
    match std::fs::read_to_string(&path) {
        Ok(s) => s.trim().parse().unwrap_or(0),
        Err(_) => 0,
    }
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

async fn dm_outbox_flush_task(
    dm_handler: DmHandler,
    endpoint: Endpoint,
    storage: Arc<Storage>,
    outbox_notify: Arc<tokio::sync::Notify>,
    token: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = outbox_notify.notified() => {}
            _ = tokio::time::sleep(OUTBOX_FLUSH_INTERVAL) => {}
        }
        let peers = match storage.get_all_outbox_peers().await {
            Ok(p) => p,
            Err(e) => {
                log::error!("[dm-outbox] failed to get peers: {e}");
                continue;
            }
        };
        for peer in peers {
            match dm_handler.flush_outbox_for_peer(&endpoint, &peer).await {
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
}

async fn push_outbox_flush_task(
    endpoint: Endpoint,
    storage: Arc<Storage>,
    my_id: String,
    token: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(PUSH_OUTBOX_FLUSH_INTERVAL) => {}
        }

        let peers = match storage.get_push_outbox_peers().await {
            Ok(p) => p,
            Err(e) => {
                log::error!("[push-outbox] failed to get peers: {e}");
                continue;
            }
        };
        for peer in peers {
            let peer_node_ids = storage
                .get_peer_transport_node_ids(&peer)
                .await
                .unwrap_or_default();
            let targets: Vec<iroh::EndpointId> = peer_node_ids
                .iter()
                .filter_map(|id| id.parse().ok())
                .collect();
            if targets.is_empty() {
                continue;
            }

            let profile_entries = storage
                .get_pending_push_profile_ids(&peer)
                .await
                .unwrap_or_default();
            let post_entries = storage
                .get_pending_push_post_ids(&peer)
                .await
                .unwrap_or_default();
            let interaction_entries = storage
                .get_pending_push_interaction_ids(&peer)
                .await
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
                if let Ok(Some(post)) = storage.get_post_by_id(post_id).await {
                    posts.push(post);
                }
                post_outbox_ids.push(*outbox_id);
            }

            let mut interactions = Vec::new();
            let mut interaction_outbox_ids = Vec::new();
            for (outbox_id, interaction_id) in &interaction_entries {
                if let Ok(Some(interaction)) = storage.get_interaction_by_id(interaction_id).await {
                    interactions.push(interaction);
                }
                interaction_outbox_ids.push(*outbox_id);
            }

            let profile = if !profile_entries.is_empty() {
                storage.get_profile(&my_id).await.ok().flatten()
            } else {
                None
            };

            let msg = iroh_social_types::PushMessage {
                author: my_id.clone(),
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
                match push::push_to_peer(&endpoint, *target, &msg).await {
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
                let _ = storage.remove_push_outbox_entries(&all_ids).await;
            } else {
                log::error!(
                    "[push-outbox] failed to push to {} (tried {} devices)",
                    short_id(&peer),
                    targets.len()
                );
                let _ = storage.mark_push_attempted(&all_ids).await;
            }
        }
    }
}

async fn housekeeping_task(storage: Arc<Storage>, token: CancellationToken) {
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(HOUSEKEEPING_INTERVAL) => {}
        }
        match storage.prune_expired_push_entries().await {
            Ok(count) if count > 0 => {
                log::info!("[housekeeping] pruned {count} expired push entries");
            }
            Err(e) => {
                log::error!("[housekeeping] push prune error: {e}");
            }
            _ => {}
        }
        match storage.prune_expired_follow_requests().await {
            Ok(count) if count > 0 => {
                log::info!("[housekeeping] pruned {count} expired follow requests");
            }
            Err(e) => {
                log::error!("[housekeeping] follow request prune error: {e}");
            }
            _ => {}
        }
    }
}

async fn device_sync_task(
    endpoint: Endpoint,
    storage: Arc<Storage>,
    master_pubkey: String,
    signing_secret_key_bytes: [u8; 32],
    token: CancellationToken,
) {
    tokio::select! {
        _ = token.cancelled() => return,
        _ = tokio::time::sleep(DEVICE_SYNC_INITIAL_DELAY) => {}
    }
    loop {
        crate::device_sync::sync_all_devices(
            &endpoint,
            &storage,
            &master_pubkey,
            &signing_secret_key_bytes,
        )
        .await;
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(DEVICE_SYNC_INTERVAL) => {}
        }
    }
}

async fn network_health_task(endpoint: Endpoint, gossip: GossipHandle, token: CancellationToken) {
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

/// Gossip follow subscriptions + startup sync + drip sync.
/// All in one task so drip naturally starts after startup-sync finishes,
/// avoiding concurrent syncs to the same peer.
#[allow(clippy::too_many_arguments)]
async fn subscribe_and_sync_task(
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

    // Derive DM key (pure X25519, independent from signing key)
    let dm_key_index: u32 = load_dm_key_index(&data_dir);
    let dm_secret_key_bytes = derive_dm_key(&master_secret_key_bytes, dm_key_index);
    let (_, dm_x25519_public) = crate::crypto::x25519_keypair_from_raw(&dm_secret_key_bytes);
    let dm_pubkey = hex::encode(dm_x25519_public);
    log::info!("[setup] DM pubkey: {}", short_id(&dm_pubkey));

    // Sign delegation binding signing key + DM key to master key
    let delegation = sign_delegation(
        &master_secret,
        &signing_secret.public(),
        signing_key_index,
        &dm_pubkey,
        dm_key_index,
        now_millis(),
    );

    // Derive stable transport key from master key (device index 0 = primary device)
    let device_index: u32 = 0;
    let transport_key_bytes = derive_transport_key(&master_secret_key_bytes, device_index);
    let transport_secret = SecretKey::from_bytes(&transport_key_bytes);

    let master_pubkey_clone = master_pubkey.clone();
    let signing_pubkey_clone = signing_pubkey.clone();
    let dm_pubkey_clone = dm_pubkey.clone();
    let delegation_clone = delegation.clone();
    let db_path = data_dir.join("social.db");
    tauri::async_runtime::block_on(async move {
        let storage_clone = Arc::new(
            Storage::open(&db_path)
                .await
                .expect("failed to open database"),
        );
        log::info!("[setup] database opened");

        let follows = storage_clone.get_follows().await.unwrap_or_default();
        log::info!("[setup] loaded {} follows", follows.len());
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

        // DM handler: dedicated X25519 key for Noise IK + Double Ratchet.
        let dm_outbox_notify = Arc::new(tokio::sync::Notify::new());
        let dm_handler = DmHandler::new(
            storage_clone.clone(),
            handle.clone(),
            dm_secret_key_bytes,
            master_pubkey_clone.clone(),
            dm_pubkey_clone.clone(),
            dm_outbox_notify.clone(),
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
            dm_secret_key_bytes,
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

        // The reconnect_tx is only needed for the initial GossipActor construction.
        // The actor's run() loop creates its own internal reconnect channel.
        let (reconnect_tx, _reconnect_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut actor = GossipActor::new(
            gossip,
            endpoint.clone(),
            master_pubkey_clone.clone(),
            storage_clone.clone(),
            handle.clone(),
            reconnect_tx,
        );

        // Start own feed and broadcast profile before spawning the actor
        if let Err(e) = actor.start_own_feed().await {
            log::error!("[setup] failed to start own feed: {e}");
        } else {
            log::info!("[setup] own gossip feed started");
        }

        if let Ok(Some(profile)) = storage_clone.get_profile(&master_pubkey_clone).await {
            if let Err(e) = actor.broadcast_profile(&profile).await {
                log::error!("[setup] failed to broadcast profile: {e}");
            } else {
                log::info!("[setup] broadcast profile: {}", profile.display_name);
            }
        }

        // Register this device and broadcast single-device announcement
        {
            let now = now_millis();
            if let Err(e) = storage_clone
                .upsert_linked_device(&transport_node_id, "Primary Device", true, true, now)
                .await
            {
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

            if let Err(e) = actor.broadcast_linked_devices(&announcement).await {
                log::error!("[setup] failed to broadcast device announcement: {e}");
            } else {
                log::info!("[setup] broadcast single-device announcement");
            }
        }

        // Spawn the actor -- returns a cloneable handle
        let gossip_handle = actor.spawn();

        let shutdown_token = CancellationToken::new();
        let (sync_tx, sync_rx) = tokio::sync::mpsc::channel(16);

        // Gossip follow subscriptions + startup sync + drip sync
        tokio::spawn(subscribe_and_sync_task(
            gossip_handle.clone(),
            endpoint.clone(),
            storage_clone.clone(),
            handle.clone(),
            master_pubkey_clone.clone(),
            follows.clone(),
            sync_rx,
            shutdown_token.child_token(),
        ));

        tokio::spawn(dm_outbox_flush_task(
            dm_handler.clone(),
            endpoint.clone(),
            storage_clone.clone(),
            dm_outbox_notify,
            shutdown_token.child_token(),
        ));

        tokio::spawn(push_outbox_flush_task(
            endpoint.clone(),
            storage_clone.clone(),
            master_pubkey_clone.clone(),
            shutdown_token.child_token(),
        ));

        tokio::spawn(housekeeping_task(
            storage_clone.clone(),
            shutdown_token.child_token(),
        ));

        tokio::spawn(device_sync_task(
            endpoint.clone(),
            storage_clone.clone(),
            master_pubkey_clone.clone(),
            signing_secret_key_bytes,
            shutdown_token.child_token(),
        ));

        tokio::spawn(network_health_task(
            endpoint.clone(),
            gossip_handle.clone(),
            shutdown_token.child_token(),
        ));

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("iroh-social/1.0")
            .build()
            .expect("failed to build HTTP client");

        let state = Arc::new(AppState {
            endpoint,
            router,
            blobs,
            store,
            storage: storage_clone,
            gossip: gossip_handle,
            dm: dm_handler,
            master_secret_key_bytes,
            master_pubkey: master_pubkey_clone.clone(),
            signing_secret_key_bytes,
            signing_key: SecretKey::from_bytes(&signing_secret_key_bytes),
            signing_pubkey: signing_pubkey_clone,
            signing_key_index,
            dm_pubkey: dm_pubkey_clone,
            dm_key_index,
            transport_node_id: transport_node_id.clone(),
            delegation: delegation_clone,
            pending_link,
            http_client,
            og_cache: crate::opengraph::OgCache::new(),
            sync_tx,
            shutdown_token,
        });

        handle.manage(state);
        log::info!("[setup] app state ready");
    });

    Ok(())
}
