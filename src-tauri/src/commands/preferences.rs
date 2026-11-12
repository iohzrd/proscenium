use crate::audio::{self, AudioDevice};
use crate::error::CmdResult;
use crate::preferences;
use crate::state::AppState;
use std::sync::Arc;
use std::time::Duration;
use tauri::{Manager, State};

/// Time limit for stopping the network stack and for shutting down the blob store.
const WIPE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Delete all local data, then restart the app.
///
/// Asks for confirmation in a native dialog first and returns the error `"cancelled"` if
/// the user declines. After confirmation:
/// 1. The network stack is stopped (`setup::stop_network`), so no peer request is
///    answered and no received message is stored while tables are empty (an absent own
///    profile row reads as public visibility).
/// 2. Every database row is deleted in one transaction.
/// 3. The blob store is shut down and its directory is removed.
/// 4. The process restarts. Key files are kept, so the identity is the same afterwards.
///
/// If the database transaction fails, nothing is deleted and the process still restarts,
/// because the endpoint is already closed.
#[tauri::command]
pub async fn wipe_all_data(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    let app = state.app_handle.clone();
    let confirmed = crate::confirm::confirm_native(
        &app,
        "Delete all data",
        "Delete all posts, messages, follows, media and settings stored on this device? This cannot be undone. The app restarts afterwards.",
        "Delete everything",
    )
    .await;
    if !confirmed {
        return Err("cancelled".into());
    }
    let data_dir = app.path().app_data_dir()?;
    let blobs_dir = data_dir.join(crate::setup::BLOBS_DIR);
    // The keys are kept, so the identity is not new: `get_seed_phrase` keeps asking for
    // confirmation although no profile remains.
    let marker = data_dir.join(crate::setup::SEED_CONFIRM_FILE);
    tokio::task::spawn_blocking(move || crate::setup::write_secret_file(&marker, b"1"))
        .await
        .map_err(|e| format!("marker write task failed: {e}"))??;

    if tokio::time::timeout(WIPE_SHUTDOWN_TIMEOUT, crate::setup::stop_network(&state))
        .await
        .is_err()
    {
        log::warn!("[wipe] stopping the network stack timed out");
    }

    if let Err(e) = state.storage.wipe_all_data().await {
        // Nothing was deleted (the deletes are one transaction). The network stack is
        // stopped, so the process restarts in either case.
        log::error!("[wipe] failed to delete database rows: {e}");
        app.request_restart();
        return Err(e);
    }
    log::info!("[wipe] database rows deleted");

    // iroh-blobs has no public call that deletes blobs from an open store. Stopping the
    // network stack closed the store, so its directory is removed; `FsStore::load`
    // creates it again at startup.
    match tokio::fs::remove_dir_all(&blobs_dir).await {
        Ok(()) => log::info!("[wipe] blob store removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::error!(
            "[wipe] failed to remove blob store {}: {e}",
            blobs_dir.display()
        ),
    }

    app.request_restart();
    Ok(())
}

/// End the application. `RunEvent::ExitRequested` stops the network first. Closing the
/// window only hides it, so this is the way to quit without a tray icon.
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub async fn get_mdns_discovery(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    state
        .storage
        .get_bool_preference(preferences::MDNS_DISCOVERY)
        .await
}

#[tauri::command]
pub async fn set_mdns_discovery(state: State<'_, Arc<AppState>>, enabled: bool) -> CmdResult<()> {
    state
        .storage
        .set_bool_preference(preferences::MDNS_DISCOVERY, enabled)
        .await
}

#[tauri::command]
pub async fn get_dht_discovery(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    state
        .storage
        .get_bool_preference(preferences::DHT_DISCOVERY)
        .await
}

#[tauri::command]
pub async fn set_dht_discovery(state: State<'_, Arc<AppState>>, enabled: bool) -> CmdResult<()> {
    state
        .storage
        .set_bool_preference(preferences::DHT_DISCOVERY, enabled)
        .await
}

#[tauri::command]
pub async fn get_share_follows(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    Ok(preferences::get_share_follows(&state.storage).await)
}

#[tauri::command]
pub async fn set_share_follows(state: State<'_, Arc<AppState>>, enabled: bool) -> CmdResult<()> {
    state
        .storage
        .set_bool_preference(preferences::SHARE_FOLLOWS, enabled)
        .await
}

#[tauri::command]
pub async fn get_share_followers(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    Ok(preferences::get_share_followers(&state.storage).await)
}

#[tauri::command]
pub async fn set_share_followers(state: State<'_, Arc<AppState>>, enabled: bool) -> CmdResult<()> {
    state
        .storage
        .set_bool_preference(preferences::SHARE_FOLLOWERS, enabled)
        .await
}

#[tauri::command]
pub fn list_audio_input_devices() -> Vec<AudioDevice> {
    audio::list_input_devices()
}

#[tauri::command]
pub fn list_audio_output_devices() -> Vec<AudioDevice> {
    audio::list_output_devices()
}

#[tauri::command]
pub async fn get_audio_input_device(state: State<'_, Arc<AppState>>) -> CmdResult<Option<String>> {
    state
        .storage
        .get_preference(preferences::AUDIO_INPUT_DEVICE)
        .await
}

#[tauri::command]
pub async fn set_audio_input_device(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> CmdResult<()> {
    // An empty name is stored as is and selects the system default device.
    state
        .storage
        .set_preference(preferences::AUDIO_INPUT_DEVICE, &name)
        .await
}

#[tauri::command]
pub async fn get_accent_color(state: State<'_, Arc<AppState>>) -> CmdResult<Option<String>> {
    state
        .storage
        .get_preference(preferences::ACCENT_COLOR)
        .await
}

#[tauri::command]
pub async fn set_accent_color(state: State<'_, Arc<AppState>>, name: String) -> CmdResult<()> {
    state
        .storage
        .set_preference(preferences::ACCENT_COLOR, &name)
        .await
}

#[tauri::command]
pub async fn get_audio_output_device(state: State<'_, Arc<AppState>>) -> CmdResult<Option<String>> {
    state
        .storage
        .get_preference(preferences::AUDIO_OUTPUT_DEVICE)
        .await
}

#[tauri::command]
pub async fn set_audio_output_device(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> CmdResult<()> {
    // An empty name is stored as is and selects the system default device.
    state
        .storage
        .set_preference(preferences::AUDIO_OUTPUT_DEVICE, &name)
        .await
}
