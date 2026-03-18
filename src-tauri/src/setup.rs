use crate::call::CallHandler;
use crate::dm::DmHandler;
use crate::gossip::GossipService;
use crate::peer::PeerHandler;
use crate::stage::StageHandler;
use crate::state::{AppState, Identity, SharedIdentity, SyncCommand};
use crate::storage::Storage;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use iroh_gossip::Gossip;
use proscenium_types::{
    CALL_ALPN, DM_ALPN, DeviceEntry, LinkedDevicesAnnouncement, PEER_ALPN, STAGE_ALPN,
    derive_dm_key, derive_signing_key, derive_transport_key, now_millis, sign_delegation,
    sign_linked_devices_announcement,
};
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::mpsc;
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
    let (dm_x25519_private, dm_x25519_public) =
        crate::crypto::x25519_keypair_from_raw(&dm_secret_key_bytes);

    let delegation = sign_delegation(
        &master_secret,
        &signing_key.public(),
        signing_key_index,
        &hex::encode(dm_x25519_public),
        dm_key_index,
        now_millis(),
    );

    // Open the database early so we can read preferences before binding the endpoint.
    let storage = Arc::new(
        Storage::open(&data_dir.join("social.db"))
            .await
            .expect("failed to open database"),
    );
    log::info!("[setup] database opened");

    // Load discovery preferences.
    let mdns_enabled = crate::preferences::get_mdns_discovery(&storage).await;
    let dht_enabled = crate::preferences::get_dht_discovery(&storage).await;
    log::info!(
        "[setup] mDNS discovery: {}, DHT discovery: {}",
        if mdns_enabled { "enabled" } else { "disabled" },
        if dht_enabled { "enabled" } else { "disabled" },
    );

    // Bind the transport endpoint (needed for transport_node_id before building Identity).
    let transport_key_bytes = derive_transport_key(&master_secret_key_bytes, 0);
    log::info!("[setup] binding iroh endpoint...");
    let mut builder = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(SecretKey::from_bytes(&transport_key_bytes))
        .alpns(vec![
            iroh_blobs::ALPN.to_vec(),
            iroh_gossip::ALPN.to_vec(),
            PEER_ALPN.to_vec(),
            DM_ALPN.to_vec(),
            CALL_ALPN.to_vec(),
            STAGE_ALPN.to_vec(),
        ]);

    if mdns_enabled {
        builder = builder.address_lookup(iroh::address_lookup::MdnsAddressLookup::builder());
    }
    if dht_enabled {
        builder = builder.address_lookup(iroh::address_lookup::DhtAddressLookup::builder());
    }

    let endpoint = builder.bind().await.expect("failed to bind iroh endpoint");

    // Derive the ratchet storage key (used by DmHandler, stored in Identity so it
    // can be updated atomically when the DM key is rotated).
    let ratchet_storage_key = crate::crypto::derive_ratchet_storage_key(&dm_secret_key_bytes);

    // Build the plain Identity struct first, then wrap in SharedIdentity.
    // This lets the rest of setup.rs use field access directly without lock overhead.
    let identity_data = Identity {
        master_secret_key_bytes,
        master_pubkey: master_secret.public().to_string(),
        signing_secret_key_bytes,
        signing_key,
        signing_key_index,
        dm_secret_key_bytes,
        dm_pubkey: hex::encode(dm_x25519_public),
        dm_key_index,
        dm_x25519_private,
        dm_x25519_public,
        ratchet_storage_key,
        transport_node_id: endpoint.id().to_string(),
        delegation,
    };

    log::info!("[setup] master pubkey: {}", &identity_data.master_pubkey);
    log::info!(
        "[setup] signing pubkey: {}",
        &identity_data.signing_key.public().to_string()
    );
    log::info!("[setup] DM pubkey: {}", &identity_data.dm_pubkey);
    log::info!(
        "[setup] Transport NodeId: {}",
        &identity_data.transport_node_id
    );
    log::info!("[setup] addr (immediate): {:?}", endpoint.addr());

    let blobs_dir = data_dir.join("blobs");
    let blob_store = FsStore::load(&blobs_dir)
        .await
        .expect("failed to open blob store");
    log::info!("[setup] blob store opened at {}", blobs_dir.display());

    let blobs = BlobsProtocol::new(&blob_store, None);
    let gossip = Gossip::builder().spawn(endpoint.clone());
    log::info!("[setup] gossip started");

    // Wrap identity now that all plain uses are done.
    let identity: SharedIdentity = Arc::new(tokio::sync::RwLock::new(identity_data));

    let gossip_service = GossipService::new(
        gossip.clone(),
        endpoint.clone(),
        identity.clone(),
        storage.clone(),
        handle.clone(),
    );

    let (call_signal_tx, call_signal_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::dm::CallSignal>();

    let mut dm_handler = DmHandler::new(
        storage.clone(),
        handle.clone(),
        endpoint.clone(),
        identity.clone(),
    );
    dm_handler.set_call_signal_tx(call_signal_tx);

    let call_handler = CallHandler::new(
        storage.clone(),
        identity.clone(),
        endpoint.clone(),
        dm_handler.clone(),
        handle.clone(),
    );

    let peer_handler = PeerHandler::new(
        storage.clone(),
        identity.clone(),
        gossip_service.clone(),
        handle.clone(),
    );

    let stage_handler = StageHandler::new(
        endpoint.clone(),
        gossip_service.gossip_handle(),
        identity.clone(),
        storage.clone(),
        gossip_service.clone(),
        handle.clone(),
    );

    let router = Router::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, blobs.clone())
        .accept(iroh_gossip::ALPN, gossip)
        .accept(PEER_ALPN, peer_handler.clone())
        .accept(DM_ALPN, dm_handler.clone())
        .accept(CALL_ALPN, call_handler.clone())
        .accept(STAGE_ALPN, stage_handler.clone())
        .spawn();
    log::info!("[setup] router spawned");

    if let Err(e) = gossip_service.start_own_feed().await {
        log::error!("[setup] failed to start own feed: {e}");
    } else {
        log::info!("[setup] own gossip feed started");
    }

    {
        let id = identity.read().await;
        if let Ok(Some(profile)) = storage.get_profile(&id.master_pubkey).await {
            drop(id);
            if let Err(e) = gossip_service.broadcast_profile(&profile).await {
                log::error!("[setup] failed to broadcast profile: {e}");
            } else {
                log::info!("[setup] broadcast profile: {}", profile.display_name);
            }
        }
    }

    {
        let id = identity.read().await;
        let now = now_millis();
        if let Err(e) = storage
            .upsert_linked_device(&id.transport_node_id, "Primary Device", true, true, now)
            .await
        {
            log::error!("[setup] failed to register own device: {e}");
        }

        let mut announcement = LinkedDevicesAnnouncement {
            master_pubkey: id.master_pubkey.clone(),
            delegation: id.delegation.clone(),
            devices: vec![DeviceEntry {
                node_id: id.transport_node_id.clone(),
                device_name: "Primary Device".to_string(),
                is_primary: true,
                added_at: now,
            }],
            version: 1,
            timestamp: now,
            signature: String::new(),
        };
        sign_linked_devices_announcement(&mut announcement, &id.signing_key);
        drop(id);

        if let Err(e) = gossip_service.broadcast_linked_devices(&announcement).await {
            log::error!("[setup] failed to broadcast device announcement: {e}");
        } else {
            log::info!("[setup] broadcast single-device announcement");
        }
    }

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("proscenium/1.0")
        .build()
        .expect("failed to build HTTP client");

    let shutdown = CancellationToken::new();
    let (sync_tx, sync_rx) = mpsc::channel::<SyncCommand>(64);

    let state = Arc::new(AppState::new(
        handle.clone(),
        identity,
        storage,
        blob_store,
        http_client,
        gossip_service,
        dm_handler,
        call_handler,
        peer_handler,
        stage_handler,
        endpoint,
        blobs,
        router,
        sync_tx,
        shutdown.clone(),
    ));

    state.gossip.start_background(shutdown.child_token()).await;
    state.dm.start_background(shutdown.child_token());
    crate::tasks::spawn_all(state.clone(), sync_rx);

    // Route call signals from DM ratchet to CallHandler
    {
        let call = state.call.clone();
        let mut rx = call_signal_rx;
        tokio::spawn(async move {
            while let Some(signal) = rx.recv().await {
                match signal.payload {
                    proscenium_types::DmPayload::CallOffer { call_id, .. } => {
                        call.on_call_offer(&call_id, &signal.peer_pubkey).await;
                    }
                    proscenium_types::DmPayload::CallAnswer { call_id } => {
                        call.on_call_answered(&call_id, &signal.peer_pubkey).await;
                    }
                    proscenium_types::DmPayload::CallReject { call_id }
                    | proscenium_types::DmPayload::CallHangup { call_id } => {
                        call.on_call_ended(&call_id).await;
                    }
                    _ => {}
                }
            }
        });
    }

    handle.manage(state);
    log::info!("[setup] app state ready");
    Ok(())
}
