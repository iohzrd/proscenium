use crate::dm::DmHandler;
use crate::gossip::GossipActor;
use crate::peer::PeerHandler;
use crate::state::AppState;
use crate::storage::Storage;
use crate::tasks;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use iroh_gossip::Gossip;
use iroh_social_types::{
    DM_ALPN, DeviceEntry, LinkedDevicesAnnouncement, PEER_ALPN, derive_dm_key, derive_signing_key,
    derive_transport_key, now_millis, short_id, sign_delegation, sign_linked_devices_announcement,
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

/// Migrate old identity.key to master_key.key if needed.
fn migrate_identity_key(data_dir: &std::path::Path) {
    let old_path = data_dir.join("identity.key");
    let new_path = data_dir.join("master_key.key");
    if old_path.exists() && !new_path.exists() {
        log::info!("[setup] migrating identity.key -> master_key.key");
        std::fs::rename(&old_path, &new_path).expect("failed to rename identity.key");
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

        // Spawn background tasks
        tokio::spawn(tasks::subscribe_and_sync_task(
            gossip_handle.clone(),
            endpoint.clone(),
            storage_clone.clone(),
            handle.clone(),
            master_pubkey_clone.clone(),
            follows.clone(),
            sync_rx,
            shutdown_token.child_token(),
        ));

        tokio::spawn(tasks::dm_outbox_flush_task(
            dm_handler.clone(),
            endpoint.clone(),
            storage_clone.clone(),
            dm_outbox_notify,
            shutdown_token.child_token(),
        ));

        tokio::spawn(tasks::push_outbox_flush_task(
            endpoint.clone(),
            storage_clone.clone(),
            master_pubkey_clone.clone(),
            shutdown_token.child_token(),
        ));

        tokio::spawn(tasks::housekeeping_task(
            storage_clone.clone(),
            shutdown_token.child_token(),
        ));

        tokio::spawn(tasks::device_sync_task(
            endpoint.clone(),
            storage_clone.clone(),
            master_pubkey_clone.clone(),
            signing_secret_key_bytes,
            shutdown_token.child_token(),
        ));

        tokio::spawn(tasks::network_health_task(
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
            identity: crate::state::Identity {
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
            },
            endpoint,
            router,
            blobs,
            store,
            storage: storage_clone,
            gossip: gossip_handle,
            dm: dm_handler,
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
