use crate::setup::save_signing_key_index;
use crate::state::AppState;
use iroh::SecretKey;
use iroh_social_types::{
    SigningKeyDelegation, derive_signing_key, now_millis, sign_delegation, sign_rotation,
};
use std::sync::Arc;
use tauri::{Manager, State};

/// Return the master key as a BIP39 24-word mnemonic.
#[tauri::command]
pub async fn get_seed_phrase(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let mnemonic = bip39::Mnemonic::from_entropy(&state.master_secret_key_bytes)
        .map_err(|e| format!("failed to generate mnemonic: {e}"))?;
    Ok(mnemonic.to_string())
}

/// Check whether the user has backed up their seed phrase.
#[tauri::command]
pub async fn is_seed_phrase_backed_up(app_handle: tauri::AppHandle) -> Result<bool, String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve data dir: {e}"))?;
    Ok(data_dir.join(".seed_backed_up").exists())
}

/// Mark the seed phrase as backed up by creating a marker file.
#[tauri::command]
pub async fn mark_seed_phrase_backed_up(app_handle: tauri::AppHandle) -> Result<(), String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve data dir: {e}"))?;
    tokio::fs::write(data_dir.join(".seed_backed_up"), b"1")
        .await
        .map_err(|e| format!("failed to write marker: {e}"))
}

/// Rotate the signing key to the next index.
/// Derives a new signing key, broadcasts the rotation via gossip, and persists
/// the new index. The app must be restarted to use the new key for signing.
#[tauri::command]
pub async fn rotate_signing_key(
    state: State<'_, Arc<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve data dir: {e}"))?;

    let old_index = state.signing_key_index;
    let new_index = old_index + 1;

    // Derive old and new signing keys
    let old_signing_bytes = derive_signing_key(&state.master_secret_key_bytes, old_index);
    let old_signing_pub = SecretKey::from_bytes(&old_signing_bytes).public();

    let new_signing_bytes = derive_signing_key(&state.master_secret_key_bytes, new_index);
    let new_signing_pub = SecretKey::from_bytes(&new_signing_bytes).public();

    let master_secret = SecretKey::from_bytes(&state.master_secret_key_bytes);
    let now = now_millis();

    // Sign new delegation with master key (DM key unchanged during signing key rotation)
    let new_delegation: SigningKeyDelegation = sign_delegation(
        &master_secret,
        &new_signing_pub,
        new_index,
        &state.dm_pubkey,
        state.dm_key_index,
        now,
    );

    // Sign the rotation announcement with master key
    let rotation = sign_rotation(
        &master_secret,
        &old_signing_pub,
        &new_signing_pub,
        new_index,
        now,
        new_delegation.clone(),
    );

    // Broadcast via gossip
    state
        .gossip
        .broadcast_signing_key_rotation(&rotation)
        .await
        .map_err(|e| format!("failed to broadcast rotation: {e}"))?;

    // Also broadcast updated linked devices announcement with new delegation
    let devices = state
        .storage
        .get_linked_devices()
        .await
        .map_err(|e| format!("failed to get devices: {e}"))?;
    let new_signing_sk = SecretKey::from_bytes(&new_signing_bytes);
    // Use own cached announcement version + 1, or device count as base
    let current_version = state
        .storage
        .get_peer_announcement_version(&state.master_pubkey)
        .await
        .unwrap_or(None)
        .unwrap_or(devices.len() as u64);
    let version = current_version + 1;
    let mut announcement = iroh_social_types::LinkedDevicesAnnouncement {
        master_pubkey: state.master_pubkey.clone(),
        delegation: new_delegation,
        devices,
        version,
        timestamp: now,
        signature: String::new(),
    };
    iroh_social_types::sign_linked_devices_announcement(&mut announcement, &new_signing_sk);
    // Cache our own announcement so version tracking works
    if let Err(e) = state
        .storage
        .cache_peer_device_announcement(&state.master_pubkey, &announcement)
        .await
    {
        log::error!("[rotate] failed to cache own announcement: {e}");
    }
    state
        .gossip
        .broadcast_linked_devices(&announcement)
        .await
        .map_err(|e| format!("failed to broadcast updated announcement: {e}"))?;

    // Re-register with all servers using the new signing key
    let servers = state
        .storage
        .get_registered_servers()
        .await
        .map_err(|e| format!("failed to get servers: {e}"))?;
    let new_signing_sk = SecretKey::from_bytes(&new_signing_bytes);
    let new_delegation_for_reg = sign_delegation(
        &master_secret,
        &new_signing_pub,
        new_index,
        &state.dm_pubkey,
        state.dm_key_index,
        now,
    );
    for server in &servers {
        let payload = iroh_social_types::RegistrationPayload {
            master_pubkey: state.master_pubkey.clone(),
            transport_node_id: state.transport_node_id.clone(),
            server_url: server.url.clone(),
            timestamp: now,
            visibility: server
                .visibility
                .parse()
                .unwrap_or(iroh_social_types::Visibility::Public),
            action: None,
        };
        let signature = iroh_social_types::sign_registration(&payload, &new_signing_sk);
        let request = iroh_social_types::RegistrationRequest {
            master_pubkey: state.master_pubkey.clone(),
            transport_node_id: state.transport_node_id.clone(),
            server_url: server.url.clone(),
            timestamp: now,
            visibility: server
                .visibility
                .parse()
                .unwrap_or(iroh_social_types::Visibility::Public),
            action: None,
            signature,
            delegation: new_delegation_for_reg.clone(),
        };
        match state
            .http_client
            .post(format!("{}/api/v1/register", server.url))
            .json(&request)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                log::info!("[rotate] re-registered with {}", server.url);
            }
            Ok(resp) => {
                log::warn!(
                    "[rotate] re-registration with {} failed: {}",
                    server.url,
                    resp.status()
                );
            }
            Err(e) => {
                log::warn!("[rotate] re-registration with {} failed: {e}", server.url);
            }
        }
    }

    // Persist the new index LAST (after successful broadcast)
    save_signing_key_index(&data_dir, new_index);
    log::info!(
        "[rotate] signing key rotated from index {old_index} to {new_index}, restart required"
    );

    Ok(format!(
        "Signing key rotated to index {new_index}. Please restart the app to use the new key."
    ))
}

/// Verify that specific words from the seed phrase are correct.
/// Takes a list of (index, word) pairs where index is 0-based.
#[tauri::command]
pub async fn verify_seed_phrase_words(
    state: State<'_, Arc<AppState>>,
    checks: Vec<(usize, String)>,
) -> Result<bool, String> {
    let mnemonic = bip39::Mnemonic::from_entropy(&state.master_secret_key_bytes)
        .map_err(|e| format!("failed to generate mnemonic: {e}"))?;
    let words: Vec<&str> = mnemonic.words().collect();
    for (idx, word) in &checks {
        if *idx >= words.len() {
            return Ok(false);
        }
        if words[*idx] != word.as_str() {
            return Ok(false);
        }
    }
    Ok(true)
}
