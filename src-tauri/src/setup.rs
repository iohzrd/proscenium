use crate::dm::DmHandler;
use crate::gossip::GossipService;
use crate::peer::PeerHandler;
use crate::state::{AppState, Identity, Net, TaskManager};
use crate::storage::Storage;
use crate::tasks;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use iroh_gossip::Gossip;
use iroh_social_types::{
    DM_ALPN, DeviceEntry, LinkedDevicesAnnouncement, PEER_ALPN, derive_dm_key, derive_signing_key,
    derive_transport_key, now_millis, sign_delegation, sign_linked_devices_announcement,
};
use std::sync::Arc;
use tauri::Manager;
use tokio_util::sync::CancellationToken;

use crate::constants::*;

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

pub fn initialize(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        use tauri_plugin_deep_link::DeepLinkExt;
        if let Err(e) = app.deep_link().register_all() {
            log::error!("[setup] failed to register deep link schemes: {e}");
        }
    }

    let handle = app.handle().clone();
    tauri::async_runtime::block_on(setup(handle))
}

async fn setup(handle: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = handle
        .path()
        .app_data_dir()
        .expect("failed to resolve app data dir");
    std::fs::create_dir_all(&data_dir).expect("failed to create app data dir");
    log::info!("[setup] data dir: {}", data_dir.display());

    // Derive all key material.
    let master_secret_key_bytes = load_or_create_key_bytes(&data_dir.join("master_key.key"));
    let master_secret = SecretKey::from_bytes(&master_secret_key_bytes);
    let signing_key_index = load_signing_key_index(&data_dir);
    let signing_secret_key_bytes = derive_signing_key(&master_secret_key_bytes, signing_key_index);
    let signing_key = SecretKey::from_bytes(&signing_secret_key_bytes);
    let dm_key_index = load_dm_key_index(&data_dir);
    let dm_secret_key_bytes = derive_dm_key(&master_secret_key_bytes, dm_key_index);
    let (_, dm_x25519_public) = crate::crypto::x25519_keypair_from_raw(&dm_secret_key_bytes);

    let delegation = sign_delegation(
        &master_secret,
        &signing_key.public(),
        signing_key_index,
        &hex::encode(dm_x25519_public),
        dm_key_index,
        now_millis(),
    );

    // Bind the transport endpoint (needed for transport_node_id before building Identity).
    let transport_key_bytes = derive_transport_key(&master_secret_key_bytes, 0);
    log::info!("[setup] binding iroh endpoint...");
    let endpoint = Endpoint::builder()
        .secret_key(SecretKey::from_bytes(&transport_key_bytes))
        .alpns(vec![
            iroh_blobs::ALPN.to_vec(),
            iroh_gossip::ALPN.to_vec(),
            PEER_ALPN.to_vec(),
            DM_ALPN.to_vec(),
        ])
        .bind()
        .await
        .expect("failed to bind iroh endpoint");

    // All ingredients are ready — build the shared identity once.
    // Everything moves in; nothing is cloned here.
    let identity = Arc::new(Identity {
        master_secret_key_bytes,
        master_pubkey: master_secret.public().to_string(),
        signing_secret_key_bytes,
        signing_key,
        signing_key_index,
        dm_secret_key_bytes,
        dm_pubkey: hex::encode(dm_x25519_public),
        dm_key_index,
        transport_node_id: endpoint.id().to_string(),
        delegation,
    });

    log::info!("[setup] master pubkey: {}", &identity.master_pubkey);
    log::info!(
        "[setup] signing pubkey: {}",
        &identity.signing_key.public().to_string()
    );
    log::info!("[setup] DM pubkey: {}", &identity.dm_pubkey);
    log::info!("[setup] Transport NodeId: {}", &identity.transport_node_id);
    log::info!("[setup] addr (immediate): {:?}", endpoint.addr());

    let ep_clone = endpoint.clone();
    tokio::spawn(async move {
        tokio::time::sleep(RELAY_LOG_DELAY).await;
        log::info!("[setup] addr (after 3s): {:?}", ep_clone.addr());
    });

    let storage = Arc::new(
        Storage::open(&data_dir.join("social.db"))
            .await
            .expect("failed to open database"),
    );
    log::info!("[setup] database opened");

    let follows = storage.get_follows().await.unwrap_or_default();
    log::info!("[setup] loaded {} follows", follows.len());

    let blobs_dir = data_dir.join("blobs");
    let blob_store = FsStore::load(&blobs_dir)
        .await
        .expect("failed to open blob store");
    log::info!("[setup] blob store opened at {}", blobs_dir.display());

    let blobs = BlobsProtocol::new(&blob_store, None);
    let gossip = Gossip::builder().spawn(endpoint.clone());
    log::info!("[setup] gossip started");

    let (gossip_service, reconnect_rx) = GossipService::new(
        gossip.clone(),
        endpoint.clone(),
        identity.master_pubkey.clone(),
        storage.clone(),
        handle.clone(),
    );

    let dm_outbox_notify = Arc::new(tokio::sync::Notify::new());
    let dm_handler = DmHandler::new(
        storage.clone(),
        handle.clone(),
        identity.dm_secret_key_bytes,
        identity.master_pubkey.clone(),
        identity.dm_pubkey.clone(),
        dm_outbox_notify.clone(),
    );

    let pending_link: crate::state::PendingLinkState = Arc::new(tokio::sync::Mutex::new(None));

    let peer_handler = PeerHandler::new(
        storage.clone(),
        identity.clone(),
        gossip_service.clone(),
        pending_link.clone(),
        handle.clone(),
    );

    let router = Router::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, blobs.clone())
        .accept(iroh_gossip::ALPN, gossip)
        .accept(PEER_ALPN, peer_handler)
        .accept(DM_ALPN, dm_handler.clone())
        .spawn();
    log::info!("[setup] router spawned");

    if let Err(e) = gossip_service.start_own_feed().await {
        log::error!("[setup] failed to start own feed: {e}");
    } else {
        log::info!("[setup] own gossip feed started");
    }

    if let Ok(Some(profile)) = storage.get_profile(&identity.master_pubkey).await {
        if let Err(e) = gossip_service.broadcast_profile(&profile).await {
            log::error!("[setup] failed to broadcast profile: {e}");
        } else {
            log::info!("[setup] broadcast profile: {}", profile.display_name);
        }
    }

    {
        let now = now_millis();
        if let Err(e) = storage
            .upsert_linked_device(
                &identity.transport_node_id,
                "Primary Device",
                true,
                true,
                now,
            )
            .await
        {
            log::error!("[setup] failed to register own device: {e}");
        }

        let mut announcement = LinkedDevicesAnnouncement {
            master_pubkey: identity.master_pubkey.clone(),
            delegation: identity.delegation.clone(),
            devices: vec![DeviceEntry {
                node_id: identity.transport_node_id.clone(),
                device_name: "Primary Device".to_string(),
                is_primary: true,
                added_at: now,
            }],
            version: 1,
            timestamp: now,
            signature: String::new(),
        };
        sign_linked_devices_announcement(&mut announcement, &identity.signing_key);

        if let Err(e) = gossip_service.broadcast_linked_devices(&announcement).await {
            log::error!("[setup] failed to broadcast device announcement: {e}");
        } else {
            log::info!("[setup] broadcast single-device announcement");
        }
    }

    let shutdown_token = CancellationToken::new();
    let (sync_tx, sync_rx) = tokio::sync::mpsc::channel(16);

    let mut task_manager = TaskManager::new();
    tasks::spawn_all(
        &mut task_manager,
        endpoint.clone(),
        gossip_service.clone(),
        dm_handler.clone(),
        storage.clone(),
        identity.clone(),
        handle.clone(),
        follows,
        reconnect_rx,
        sync_rx,
        dm_outbox_notify,
        shutdown_token.clone(),
    );

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("iroh-social/1.0")
        .build()
        .expect("failed to build HTTP client");

    let state = Arc::new(AppState {
        identity,
        net: Net::new(endpoint, gossip_service, dm_handler, blobs, router),
        storage,
        blob_store,
        http_client,
        og_cache: crate::opengraph::OgCache::new(),
        pending_link,
        sync_tx,
        shutdown_token,
        task_manager: tokio::sync::Mutex::new(Some(task_manager)),
    });

    handle.manage(state);
    log::info!("[setup] app state ready");
    Ok(())
}
