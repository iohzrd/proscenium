use crate::call::CallHandler;
use crate::dm::DmHandler;
use crate::error::AppError;
use crate::gossip::GossipService;
use crate::peer::PeerHandler;
use crate::stage::StageHandler;
use crate::state::{AppState, Identity, NetworkStack, SharedIdentity, SyncCommand};
use crate::storage::Storage;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use iroh_gossip::Gossip;
use proscenium_types::{
    CALL_ALPN, DM_ALPN, DeviceEntry, LinkBundleData, PEER_ALPN, STAGE_ALPN, now_millis,
};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::Manager;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroize;

const MASTER_KEY_FILE: &str = "master_key.key";
/// Secret key of this device's iroh endpoint.
const TRANSPORT_KEY_FILE: &str = "transport_key.key";
/// Directory of the blob store inside the data directory.
pub(crate) const BLOBS_DIR: &str = "blobs";
/// Marker file. While it exists, `get_seed_phrase` asks for native confirmation.
pub(crate) const SEED_BACKED_UP_FILE: &str = ".seed_backed_up";
/// Marker file written when the stored data of an existing identity is deleted and the
/// keys are kept. While it exists, `get_seed_phrase` asks for native confirmation.
pub(crate) const SEED_CONFIRM_FILE: &str = ".seed_confirm";
/// Suffix of the temporary files created by `write_secret_file`.
const TEMP_FILE_SUFFIX: &str = ".tmp";

/// Time limit for the network shutdown when the application exits.
const EXIT_SHUTDOWN_LIMIT: Duration = Duration::from_secs(3);
/// Time limit for the network shutdown before the local identity is replaced.
const REPLACE_SHUTDOWN_LIMIT: Duration = Duration::from_secs(10);

/// The master key and transport key that define the identity of a device. Both are
/// overwritten with zeros when the value is dropped.
pub struct IdentityKeys {
    pub master_secret_key: [u8; 32],
    pub transport_secret_key: [u8; 32],
}

impl Drop for IdentityKeys {
    fn drop(&mut self) {
        self.master_secret_key.zeroize();
        self.transport_secret_key.zeroize();
    }
}

/// 32 random bytes for a new master or transport key.
pub(crate) fn random_key() -> Result<[u8; 32], AppError> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key)?;
    Ok(key)
}

/// Write `bytes` to `path` so that `path` holds either its previous content or all of
/// `bytes`.
///
/// Creates a temporary file in the same directory (mode 0600 on Unix), writes it, calls
/// fsync, renames it over `path`, then calls fsync on the directory (Unix).
pub(crate) fn write_secret_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent directory"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("path has no file name"))?;
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(std::io::Error::other)?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".{}{TEMP_FILE_SUFFIX}", hex::encode(random)));
    let tmp_path = dir.join(tmp_name);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let written = (|| {
        let mut file = options.open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return written;
    }

    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Delete temporary files that `write_secret_file` left behind when the process ended
/// between creating the file and renaming it. Such a file can hold key material.
fn remove_stale_temp_files(data_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_temp = name.ends_with(TEMP_FILE_SUFFIX)
            && [
                MASTER_KEY_FILE,
                TRANSPORT_KEY_FILE,
                SEED_BACKED_UP_FILE,
                SEED_CONFIRM_FILE,
            ]
            .iter()
            .any(|prefix| name.starts_with(&format!("{prefix}.")));
        if is_temp && let Err(e) = std::fs::remove_file(entry.path()) {
            log::warn!("[setup] failed to delete temporary file {name}: {e}");
        }
    }
}

/// Remove the permission bits of `path` that are not in `mode`.
#[cfg(unix)]
fn restrict_permissions(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let current = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if current & !mode != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(current & mode))?;
    }
    Ok(())
}

/// Create the data directory. On Unix a new directory gets mode 0700, and an existing
/// directory loses its group and other permission bits.
fn create_data_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;

    #[cfg(unix)]
    if let Err(e) = restrict_permissions(path, 0o700) {
        log::warn!(
            "[setup] failed to restrict permissions of {}: {e}",
            path.display()
        );
    }
    Ok(())
}

/// Read a key file. Returns `Ok(None)` if the file does not exist, and an error if it
/// cannot be read or does not hold exactly 32 bytes.
fn read_key_file(path: &Path) -> Result<Option<[u8; 32]>, AppError> {
    let mut bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(AppError::Other(format!(
                "failed to read {}: {e}",
                path.display()
            )));
        }
    };
    let key: Result<[u8; 32], _> = bytes.as_slice().try_into();
    let len = bytes.len();
    bytes.zeroize();
    let key = key.map_err(|_| {
        AppError::Other(format!(
            "{} holds {len} bytes, expected 32. Restore the file from a backup.",
            path.display()
        ))
    })?;

    #[cfg(unix)]
    if let Err(e) = restrict_permissions(path, 0o600) {
        log::warn!(
            "[setup] failed to restrict permissions of {}: {e}",
            path.display()
        );
    }
    Ok(Some(key))
}

/// Write a new random key to `path`.
fn write_random_key_file(path: &Path) -> Result<(), AppError> {
    let mut key = random_key()?;
    let written = write_secret_file(path, &key);
    key.zeroize();
    written.map_err(|e| AppError::Other(format!("failed to write {}: {e}", path.display())))
}

/// Make sure the master key file and the transport key file exist.
///
/// A missing transport key file gets a new random key; the device then has a new NodeId.
/// A new random master key is generated only if the file is missing and the database
/// holds no rows of an existing identity. A missing master key file next to such a
/// database is an error: generating a key would start a different identity on top of the
/// stored data.
async fn ensure_key_files(data_dir: &Path, storage: &Storage) -> Result<(), AppError> {
    let transport_path = data_dir.join(TRANSPORT_KEY_FILE);
    if read_key_file(&transport_path)?.is_none() {
        write_random_key_file(&transport_path)?;
        log::info!("[setup] generated a new transport key");
    }

    let path = data_dir.join(MASTER_KEY_FILE);
    if read_key_file(&path)?.is_some() {
        return Ok(());
    }
    if storage.has_identity_data().await? {
        return Err(AppError::Other(format!(
            "{} is missing but the database holds data of an existing identity. \
             Restore the key file, or remove social.db to start with a new identity.",
            path.display()
        )));
    }
    write_random_key_file(&path)?;
    log::info!("[setup] generated a new master key");
    Ok(())
}

/// Read the master key and transport key files in `data_dir`.
fn load_identity(data_dir: &Path) -> Result<Identity, AppError> {
    let read = |name: &str| {
        let path = data_dir.join(name);
        read_key_file(&path)?
            .ok_or_else(|| AppError::Other(format!("{} is missing", path.display())))
    };
    let master_secret_key_bytes = read(MASTER_KEY_FILE)?;
    let mut transport_secret_key = read(TRANSPORT_KEY_FILE)?;
    let transport_key = SecretKey::from_bytes(&transport_secret_key);
    transport_secret_key.zeroize();
    Ok(Identity::new(master_secret_key_bytes, transport_key))
}

/// Write the transport key file, then the master key file.
///
/// The master key is written last: if a step fails, the previous master key is still on
/// disk, used with either transport key.
fn write_identity_files(data_dir: &Path, keys: &IdentityKeys) -> std::io::Result<()> {
    write_secret_file(
        &data_dir.join(TRANSPORT_KEY_FILE),
        &keys.transport_secret_key,
    )?;
    write_secret_file(&data_dir.join(MASTER_KEY_FILE), &keys.master_secret_key)
}

/// Run `write` on the blocking thread pool and flatten the join error into `AppError`.
async fn run_blocking_io<F>(write: F) -> Result<(), AppError>
where
    F: FnOnce() -> std::io::Result<()> + Send + 'static,
{
    tokio::task::spawn_blocking(write)
        .await
        .map_err(|e| AppError::Other(format!("file write task failed: {e}")))?
        .map_err(AppError::from)
}

/// Build the networking stack: blob store, endpoint, gossip, handlers, router.
/// The blob store is opened at `blobs_dir`; `Router::shutdown` closes it. The endpoint key
/// is the transport key of `identity`.
/// Returns the stack plus channel receivers needed by background tasks.
async fn build_network_stack(
    handle: &tauri::AppHandle,
    identity: SharedIdentity,
    storage: Arc<Storage>,
    blobs_dir: &Path,
) -> Result<
    (
        NetworkStack,
        mpsc::Receiver<SyncCommand>,
        mpsc::UnboundedReceiver<crate::dm::CallSignal>,
    ),
    AppError,
> {
    let blob_store = FsStore::load(blobs_dir)
        .await
        .map_err(|e| AppError::Other(format!("failed to open blob store: {e}")))?;
    log::info!("[setup] blob store opened at {}", blobs_dir.display());

    let mdns_enabled = crate::preferences::get_mdns_discovery(&storage).await;
    let dht_enabled = crate::preferences::get_dht_discovery(&storage).await;
    log::info!(
        "[setup] mDNS discovery: {}, DHT discovery: {}",
        if mdns_enabled { "enabled" } else { "disabled" },
        if dht_enabled { "enabled" } else { "disabled" },
    );

    let transport_secret_key = identity.read().await.transport_key.clone();
    log::info!("[setup] binding iroh endpoint...");
    let mut builder = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(transport_secret_key)
        .alpns(vec![
            iroh_blobs::ALPN.to_vec(),
            iroh_gossip::ALPN.to_vec(),
            PEER_ALPN.to_vec(),
            DM_ALPN.to_vec(),
            CALL_ALPN.to_vec(),
            STAGE_ALPN.to_vec(),
        ]);

    if mdns_enabled {
        builder = builder.address_lookup(iroh_mdns_address_lookup::MdnsAddressLookup::builder());
    }
    if dht_enabled {
        builder = builder.address_lookup(iroh_mainline_address_lookup::DhtAddressLookup::builder());
    }

    let endpoint = builder
        .bind()
        .await
        .map_err(|e| AppError::Other(format!("failed to bind iroh endpoint: {e}")))?;

    let blobs = BlobsProtocol::new(&blob_store, None);
    let gossip = Gossip::builder()
        .max_message_size(crate::constants::GOSSIP_MAX_MESSAGE_SIZE)
        .spawn(endpoint.clone());
    log::info!("[setup] gossip started");

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
        endpoint.clone(),
        dm_handler.clone(),
        handle.clone(),
    );

    let stage_handler = StageHandler::new(
        endpoint.clone(),
        gossip_service.gossip_handle(),
        identity.clone(),
        gossip_service.clone(),
        handle.clone(),
    );

    let peer_handler = PeerHandler::new(
        storage.clone(),
        identity.clone(),
        gossip_service.clone(),
        handle.clone(),
        stage_handler.actor_handle(),
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

    let shutdown = CancellationToken::new();
    let (sync_tx, sync_rx) = mpsc::channel::<SyncCommand>(64);

    let net = NetworkStack {
        blob_store,
        endpoint,
        gossip: gossip_service,
        dm: dm_handler,
        call: call_handler,
        peer: peer_handler,
        stage: stage_handler,
        blobs,
        sync_tx,
        shutdown,
        router,
    };

    Ok((net, sync_rx, call_signal_rx))
}

/// Start background services on the networking stack.
async fn start_services(
    state: &Arc<AppState>,
    sync_rx: mpsc::Receiver<SyncCommand>,
    mut call_signal_rx: mpsc::UnboundedReceiver<crate::dm::CallSignal>,
) {
    let shutdown = state.shutdown();
    state
        .gossip()
        .start_background(shutdown.child_token())
        .await;
    state.dm().start_background(shutdown.child_token());
    crate::tasks::spawn_all(state.clone(), sync_rx);

    // Forwards call signals from the DM handler to the call handler. The task holds a
    // `CallHandler`, which holds a sender of this channel, so `recv` never yields `None`;
    // the task ends when the shutdown token of this stack is cancelled.
    let call = state.call();
    let token = shutdown.child_token();
    tokio::spawn(async move {
        loop {
            let signal = tokio::select! {
                _ = token.cancelled() => break,
                signal = call_signal_rx.recv() => match signal {
                    Some(signal) => signal,
                    None => break,
                },
            };
            match signal.payload {
                proscenium_types::DmPayload::CallOffer { call_id, .. } => {
                    call.on_call_offer(&call_id, &signal.peer_pubkey, signal.peer_device)
                        .await;
                }
                proscenium_types::DmPayload::CallAnswer { call_id } => {
                    call.on_call_answered(&call_id, &signal.peer_pubkey, signal.peer_device)
                        .await;
                }
                proscenium_types::DmPayload::CallReject { call_id }
                | proscenium_types::DmPayload::CallHangup { call_id } => {
                    call.on_call_ended(&call_id, &signal.peer_pubkey).await;
                }
                _ => {}
            }
        }
    });
}

/// Broadcast initial gossip state (own feed, profile, device announcement).
async fn broadcast_initial_state(state: &Arc<AppState>) {
    let gossip = state.gossip();

    if let Err(e) = gossip.start_own_feed().await {
        log::error!("[setup] failed to start own feed: {e}");
    } else {
        log::info!("[setup] own gossip feed started");
    }

    let (transport_node_id, master_pubkey) = {
        let id = state.identity.read().await;
        (id.transport_node_id.clone(), id.master_pubkey.clone())
    };

    if let Err(e) = state
        .storage
        .upsert_linked_device(
            &transport_node_id,
            &crate::announcement::local_device_name(),
            true,
            now_millis(),
        )
        .await
    {
        log::error!("[setup] failed to register own device: {e}");
    }
    let cutoff = now_millis().saturating_sub(crate::constants::PEER_ANNOUNCEMENT_RETENTION_MS);
    match state
        .storage
        .prune_peer_announcements(&master_pubkey, cutoff)
        .await
    {
        Ok(0) => {}
        Ok(n) => log::info!("[setup] deleted {n} stale device lists of unrelated users"),
        Err(e) => log::error!("[setup] failed to delete stale device lists: {e}"),
    }

    // Signs the stored profile again if its signature does not verify under the master
    // key, then broadcasts the profile and the device list.
    match crate::announcement::build_own_identity_response(&state.storage, &state.identity).await {
        Ok(identity) => {
            if let Some(profile) = &identity.profile {
                if let Err(e) = gossip.broadcast_profile(profile).await {
                    log::error!("[setup] failed to broadcast profile: {e}");
                } else {
                    log::info!("[setup] broadcast profile: {}", profile.display_name);
                }
            }
            if let Err(e) = gossip
                .broadcast_linked_devices(&identity.announcement)
                .await
            {
                log::error!("[setup] failed to broadcast device announcement: {e}");
            } else {
                log::info!(
                    "[setup] broadcast device announcement for {} ({} devices)",
                    proscenium_types::short_id(&master_pubkey),
                    identity.announcement.devices.len()
                );
            }
        }
        Err(e) => log::error!("[setup] failed to build device announcement: {e}"),
    }
}

/// Stop the current network stack.
///
/// Cancels the shutdown token (background loops), aborts the gossip listener tasks, then
/// awaits `Router::shutdown`, which stops the protocol handlers, aborts the running
/// `accept` futures and closes the endpoint. Returns at once if the stack is already
/// stopped.
pub(crate) async fn stop_network(state: &AppState) {
    let token = state.shutdown();
    let router = state.router();
    if token.is_cancelled() && router.is_shutdown() {
        return;
    }
    token.cancel();

    // The listener tasks store received gossip messages. They are aborted here so that
    // a message buffered in a topic receiver is not stored after this function returns.
    let gossip = state.gossip();
    gossip.stop_own_feed().await;
    let my_id = state.identity.read().await.master_pubkey.clone();
    match state.storage.get_follows(&my_id).await {
        Ok(follows) => {
            for follow in &follows {
                gossip.unfollow_user(&follow.pubkey).await;
            }
        }
        Err(e) => log::warn!("[net] failed to load follows for gossip unsubscribe: {e}"),
    }

    if let Err(e) = router.shutdown().await {
        log::warn!("[net] router task ended with an error: {e}");
    }
    log::info!("[net] network stack stopped");
}

/// Load the identity from the files in `data_dir`, build a network stack for it, store
/// both in `state`, start the background services and broadcast the initial state.
async fn start_network(state: &Arc<AppState>, data_dir: &Path) -> Result<(), AppError> {
    let identity = load_identity(data_dir)?;
    log::info!("[identity] master pubkey: {}", identity.master_pubkey);
    log::info!(
        "[identity] transport NodeId: {}",
        identity.transport_node_id
    );
    *state.identity.write().await = identity;

    let (net, sync_rx, call_signal_rx) = build_network_stack(
        &state.app_handle,
        state.identity.clone(),
        state.storage.clone(),
        &data_dir.join(BLOBS_DIR),
    )
    .await?;
    state.replace_net(net);

    start_services(state, sync_rx, call_signal_rx).await;
    broadcast_initial_state(state).await;
    Ok(())
}

/// Delete all rows, write the identity files, import `bundle`, and start a network stack
/// for the new identity. The previous network stack must already be stopped.
async fn install_identity(
    state: &Arc<AppState>,
    keys: IdentityKeys,
    bundle: Option<&LinkBundleData>,
) -> Result<(), AppError> {
    let data_dir = state.app_handle.path().app_data_dir()?;
    let master_pubkey = SecretKey::from_bytes(&keys.master_secret_key)
        .public()
        .to_string();
    let own_device = DeviceEntry {
        node_id: SecretKey::from_bytes(&keys.transport_secret_key)
            .public()
            .to_string(),
        device_name: crate::announcement::local_device_name(),
        added_at: now_millis(),
    };

    state.storage.wipe_all_data().await?;
    // The stopped network stack closed the blob store; the next stack opens an empty one.
    match tokio::fs::remove_dir_all(data_dir.join(BLOBS_DIR)).await {
        Ok(()) => log::info!("[identity] blob store removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }

    let dir = data_dir.clone();
    run_blocking_io(move || {
        write_identity_files(&dir, &keys)?;
        // The identity exists outside this device (on the linking device, or as the
        // recovery phrase the user entered). With the marker present, `get_seed_phrase`
        // returns the phrase only after native confirmation.
        write_secret_file(&dir.join(SEED_BACKED_UP_FILE), b"1")
    })
    .await?;

    if let Some(bundle) = bundle {
        state
            .storage
            .import_link_bundle(&master_pubkey, &own_device, bundle)
            .await?;
    }

    start_network(state, &data_dir).await
}

/// Sets `AppState::replacing_identity` for its lifetime and clears it when dropped.
struct ReplacementInProgress<'a>(&'a AppState);

impl<'a> ReplacementInProgress<'a> {
    /// Returns `None` if the flag is already set.
    fn begin(state: &'a AppState) -> Option<Self> {
        state
            .replacing_identity
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self(state))
    }
}

impl Drop for ReplacementInProgress<'_> {
    fn drop(&mut self) {
        self.0.replacing_identity.store(false, Ordering::SeqCst);
    }
}

/// Replace the local identity with the one defined by `keys` and rebuild the network
/// stack in place. Used by seed phrase recovery (`bundle` is `None`) and device linking.
///
/// Order: stop the network stack (which closes the blob store), delete all rows and the
/// blob store, write the transport key file and then the master key file, import
/// `bundle`, load the identity from disk, build and start a new network stack. Returns an
/// error, and changes nothing, if the network stack does not stop within
/// `REPLACE_SHUTDOWN_LIMIT`. The returned error of any later step states that a restart
/// is required. Returns an error at once if another call is in progress.
pub async fn replace_identity(
    state: &Arc<AppState>,
    keys: IdentityKeys,
    bundle: Option<&LinkBundleData>,
) -> Result<(), AppError> {
    let Some(_replacement) = ReplacementInProgress::begin(state) else {
        return Err("another identity replacement is in progress".into());
    };
    log::info!("[identity] replacing the local identity...");

    if tokio::time::timeout(REPLACE_SHUTDOWN_LIMIT, stop_network(state))
        .await
        .is_err()
    {
        log::error!(
            "[identity] network stack did not stop within {}s",
            REPLACE_SHUTDOWN_LIMIT.as_secs()
        );
        return Err(AppError::Other(
            "The network did not stop in time; nothing was changed. Restart the app and try again."
                .into(),
        ));
    }

    install_identity(state, keys, bundle).await.map_err(|e| {
        log::error!("[identity] replacement failed: {e}");
        AppError::Other(format!(
            "Replacing the identity failed after the network was stopped: {e}. Restart the app."
        ))
    })?;

    log::info!("[identity] identity replaced, network stack rebuilt");
    Ok(())
}

/// Stop the network stack, which closes the blob store, before the process exits, waiting
/// at most `EXIT_SHUTDOWN_LIMIT`. Peers then receive a QUIC close instead of running into
/// their idle timeout. Only the first call does anything.
///
/// Blocks the calling thread. Must not be called from a Tokio worker thread.
pub fn shutdown_for_exit(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let state = Arc::clone(state.inner());
    if state.exit_started.swap(true, Ordering::SeqCst) {
        return;
    }

    tauri::async_runtime::block_on(async move {
        if tokio::time::timeout(EXIT_SHUTDOWN_LIMIT, stop_network(&state))
            .await
            .is_err()
        {
            log::warn!(
                "[exit] network shutdown did not finish within {}s",
                EXIT_SHUTDOWN_LIMIT.as_secs()
            );
        }
    });
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
    let data_dir = handle.path().app_data_dir()?;
    create_data_dir(&data_dir).map_err(|e| {
        format!(
            "failed to create data directory {}: {e}",
            data_dir.display()
        )
    })?;
    log::info!("[setup] data dir: {}", data_dir.display());
    remove_stale_temp_files(&data_dir);

    // The database is opened before the key is loaded: `ensure_key_files` reads it to
    // tell a first start from a missing key file, and the endpoint builder reads
    // preferences from it.
    let storage = Arc::new(
        Storage::open(&data_dir.join("social.db"))
            .await
            .map_err(|e| format!("failed to open database: {e}"))?,
    );
    log::info!("[setup] database opened");

    ensure_key_files(&data_dir, &storage).await?;
    let identity_data = load_identity(&data_dir)?;

    log::info!("[setup] master pubkey: {}", identity_data.master_pubkey);
    log::info!(
        "[setup] transport NodeId: {}",
        identity_data.transport_node_id
    );

    // The handlers of the network stack hold the shared identity, so it is created first.
    let identity: SharedIdentity = Arc::new(tokio::sync::RwLock::new(identity_data));

    let (net, sync_rx, call_signal_rx) = build_network_stack(
        &handle,
        identity.clone(),
        storage.clone(),
        &data_dir.join(BLOBS_DIR),
    )
    .await?;

    log::info!("[setup] addr (immediate): {:?}", net.endpoint.addr());

    // Redirects are not followed: a discovery server must not be able to forward a
    // signed request to another host.
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("proscenium/1.0")
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let state = Arc::new(AppState::new(
        handle.clone(),
        identity,
        storage,
        http_client,
        net,
    ));

    start_services(&state, sync_rx, call_signal_rx).await;
    broadcast_initial_state(&state).await;

    handle.manage(state);
    log::info!("[setup] app state ready");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new empty directory under the system temp directory, removed on drop.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let mut random = [0u8; 8];
            getrandom::fill(&mut random).unwrap();
            let path =
                std::env::temp_dir().join(format!("proscenium-test-{}", hex::encode(random)));
            create_data_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn write_secret_file_replaces_content_and_leaves_no_temp_file() {
        let dir = TempDir::new();
        let path = dir.0.join(MASTER_KEY_FILE);
        write_secret_file(&path, &[1u8; 32]).unwrap();
        write_secret_file(&path, &[2u8; 32]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), vec![2u8; 32]);
        let names: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(MASTER_KEY_FILE)]);
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_and_data_dir_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new();
        let path = dir.0.join(MASTER_KEY_FILE);
        write_secret_file(&path, &[1u8; 32]).unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode & 0o077, 0);
        let dir_mode = std::fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode & 0o077, 0);

        // An existing file with wider permissions is narrowed when it is read.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        read_key_file(&path).unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
    }

    #[test]
    fn master_key_with_wrong_length_is_an_error() {
        let dir = TempDir::new();
        let path = dir.0.join(MASTER_KEY_FILE);
        assert!(read_key_file(&path).unwrap().is_none());
        std::fs::write(&path, [7u8; 31]).unwrap();
        let err = read_key_file(&path).unwrap_err().to_string();
        assert!(err.contains("31 bytes"), "{err}");
        assert!(load_identity(&dir.0).is_err());
    }

    #[test]
    fn identity_files_hold_the_master_and_transport_keys() {
        let dir = TempDir::new();
        let (master, transport) = ([9u8; 32], [10u8; 32]);
        write_identity_files(
            &dir.0,
            &IdentityKeys {
                master_secret_key: master,
                transport_secret_key: transport,
            },
        )
        .unwrap();
        let identity = load_identity(&dir.0).unwrap();
        assert_eq!(identity.master_secret_key_bytes, master);
        assert_eq!(
            identity.transport_node_id,
            SecretKey::from_bytes(&transport).public().to_string()
        );
        assert_eq!(identity.transport_key.to_bytes(), transport);

        // A missing transport key file is an error.
        std::fs::remove_file(dir.0.join(TRANSPORT_KEY_FILE)).unwrap();
        assert!(load_identity(&dir.0).is_err());
    }

    #[test]
    fn stale_temp_files_are_removed() {
        let dir = TempDir::new();
        let stale = dir
            .0
            .join(format!("{MASTER_KEY_FILE}.0011223344556677.tmp"));
        std::fs::write(&stale, [1u8; 32]).unwrap();
        let other = dir.0.join("notes.tmp");
        std::fs::write(&other, b"x").unwrap();
        remove_stale_temp_files(&dir.0);
        assert!(!stale.exists());
        assert!(other.exists());
    }

    #[tokio::test]
    async fn missing_master_key_with_identity_rows_is_an_error() {
        let dir = TempDir::new();
        let storage = Storage::open_in_memory().await.unwrap();
        let read = |name: &str| read_key_file(&dir.0.join(name)).unwrap().unwrap();

        // Empty database: both keys are generated.
        ensure_key_files(&dir.0, &storage).await.unwrap();
        let (master, transport) = (read(MASTER_KEY_FILE), read(TRANSPORT_KEY_FILE));
        assert_ne!(master, transport);

        // Existing keys: kept.
        ensure_key_files(&dir.0, &storage).await.unwrap();
        assert_eq!(read(MASTER_KEY_FILE), master);
        assert_eq!(read(TRANSPORT_KEY_FILE), transport);

        // A removed transport key is replaced by a new one.
        std::fs::remove_file(dir.0.join(TRANSPORT_KEY_FILE)).unwrap();
        ensure_key_files(&dir.0, &storage).await.unwrap();
        assert_ne!(read(TRANSPORT_KEY_FILE), transport);
        assert_eq!(read(MASTER_KEY_FILE), master);

        // Master key file removed while the database holds the local device row: error,
        // no new key.
        storage
            .upsert_linked_device(&node_id_of(&transport), "Linux", true, 1)
            .await
            .unwrap();
        std::fs::remove_file(dir.0.join(MASTER_KEY_FILE)).unwrap();
        assert!(ensure_key_files(&dir.0, &storage).await.is_err());
        assert!(!dir.0.join(MASTER_KEY_FILE).exists());
    }

    fn node_id_of(transport: &[u8; 32]) -> String {
        SecretKey::from_bytes(transport).public().to_string()
    }

    #[test]
    fn identity_debug_output_holds_no_secret_bytes() {
        let master = [0xABu8; 32];
        let identity = Identity::new(master, SecretKey::from_bytes(&[0xCDu8; 32]));
        let debug = format!("{identity:?}");
        assert!(debug.contains(&identity.master_pubkey));
        assert!(debug.contains(&identity.transport_node_id));
        for secret in [
            identity.master_secret_key_bytes,
            identity.transport_key.to_bytes(),
        ] {
            assert!(!debug.contains(&hex::encode(secret)));
            assert!(!debug.contains(&format!("{secret:?}")));
        }
    }
}
