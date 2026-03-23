use crate::error::CmdResult;
use crate::state::AppState;
use iroh::SecretKey;
use proscenium_types::{
    SigningKeyDelegation, derive_signing_key, now_millis, sign_delegation, sign_rotation,
};
use std::str::FromStr;
use std::sync::Arc;
use tauri::{Manager, State};

#[tauri::command]
pub async fn get_seed_phrase(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    let master_secret_key_bytes = state.identity.read().await.master_secret_key_bytes;
    let mnemonic = bip39::Mnemonic::from_entropy(&master_secret_key_bytes)?;
    Ok(mnemonic.to_string())
}

#[tauri::command]
pub async fn is_seed_phrase_backed_up(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    let data_dir = state.app_handle.path().app_data_dir()?;
    Ok(data_dir.join(".seed_backed_up").exists())
}

#[tauri::command]
pub async fn mark_seed_phrase_backed_up(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    let data_dir = state.app_handle.path().app_data_dir()?;
    tokio::fs::write(data_dir.join(".seed_backed_up"), b"1").await?;
    Ok(())
}

#[tauri::command]
pub async fn rotate_signing_key(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    let data_dir = state.app_handle.path().app_data_dir()?;

    let (
        old_index,
        master_secret_key_bytes,
        dm_pubkey,
        dm_key_index,
        master_pubkey,
        transport_node_id,
    ) = {
        let id = state.identity.read().await;
        (
            id.signing_key_index,
            id.master_secret_key_bytes,
            id.dm_pubkey.clone(),
            id.dm_key_index,
            id.master_pubkey.clone(),
            id.transport_node_id.clone(),
        )
    };

    let new_index = old_index + 1;

    let old_signing_bytes = derive_signing_key(&master_secret_key_bytes, old_index);
    let old_signing_pub = SecretKey::from_bytes(&old_signing_bytes).public();

    let new_signing_bytes = derive_signing_key(&master_secret_key_bytes, new_index);
    let new_signing_pub = SecretKey::from_bytes(&new_signing_bytes).public();

    let master_secret = SecretKey::from_bytes(&master_secret_key_bytes);
    let now = now_millis();

    let new_delegation: SigningKeyDelegation = sign_delegation(
        &master_secret,
        &new_signing_pub,
        new_index,
        &dm_pubkey,
        dm_key_index,
        now,
    );

    let rotation = sign_rotation(
        &master_secret,
        &old_signing_pub,
        &new_signing_pub,
        new_index,
        now,
        new_delegation.clone(),
    );

    state
        .gossip()
        .broadcast_signing_key_rotation(&rotation)
        .await?;

    let devices = state.storage.get_linked_devices().await?;
    let new_signing_sk = SecretKey::from_bytes(&new_signing_bytes);
    let current_version = state
        .storage
        .get_peer_announcement_version(&master_pubkey)
        .await
        .unwrap_or(None)
        .unwrap_or(devices.len() as u64);
    let version = current_version + 1;
    let mut announcement = proscenium_types::LinkedDevicesAnnouncement {
        master_pubkey: master_pubkey.clone(),
        delegation: new_delegation.clone(),
        devices,
        version,
        timestamp: now,
        signature: String::new(),
    };
    proscenium_types::sign_linked_devices_announcement(&mut announcement, &new_signing_sk);
    if let Err(e) = state
        .storage
        .cache_peer_device_announcement(&master_pubkey, &announcement)
        .await
    {
        log::error!("[rotate] failed to cache own announcement: {e}");
    }
    state
        .gossip()
        .broadcast_linked_devices(&announcement)
        .await?;

    let servers = state.storage.get_registered_servers().await?;
    let new_signing_sk = SecretKey::from_bytes(&new_signing_bytes);
    let new_delegation_for_reg = sign_delegation(
        &master_secret,
        &new_signing_pub,
        new_index,
        &dm_pubkey,
        dm_key_index,
        now,
    );
    for server in &servers {
        let payload = proscenium_types::RegistrationPayload {
            master_pubkey: master_pubkey.clone(),
            transport_node_id: transport_node_id.clone(),
            server_url: server.url.clone(),
            timestamp: now,
            visibility: server
                .visibility
                .parse()
                .unwrap_or(proscenium_types::Visibility::Public),
            action: None,
        };
        let signature = proscenium_types::sign_registration(&payload, &new_signing_sk);
        let request = proscenium_types::RegistrationRequest {
            master_pubkey: master_pubkey.clone(),
            transport_node_id: transport_node_id.clone(),
            server_url: server.url.clone(),
            timestamp: now,
            visibility: server
                .visibility
                .parse()
                .unwrap_or(proscenium_types::Visibility::Public),
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

    let signing_key_index_path = data_dir.join("signing_key_index");
    std::fs::write(&signing_key_index_path, new_index.to_string())?;

    {
        let mut id = state.identity.write().await;
        id.signing_key_index = new_index;
        id.signing_secret_key_bytes = new_signing_bytes;
        id.signing_key = SecretKey::from_bytes(&new_signing_bytes);
        id.delegation = new_delegation;
    }

    log::info!("[rotate] signing key rotated from index {old_index} to {new_index}");
    Ok(format!("Signing key rotated to index {new_index}."))
}

#[tauri::command]
pub async fn verify_seed_phrase_words(
    state: State<'_, Arc<AppState>>,
    checks: Vec<(usize, String)>,
) -> CmdResult<bool> {
    let master_secret_key_bytes = state.identity.read().await.master_secret_key_bytes;
    let mnemonic = bip39::Mnemonic::from_entropy(&master_secret_key_bytes)?;
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

/// Recover identity from a BIP39 recovery phrase.
/// Validates the phrase, extracts the master key entropy, overwrites the master key file,
/// resets derived key indices, wipes social data, and rebuilds the networking stack in place.
#[tauri::command]
pub async fn recover_from_seed_phrase(
    state: State<'_, Arc<AppState>>,
    phrase: String,
) -> CmdResult<()> {
    let mnemonic = bip39::Mnemonic::from_str(phrase.trim())
        .map_err(|e| format!("Invalid recovery phrase: {e}"))?;

    let entropy = mnemonic.to_entropy();
    let master_key_bytes: [u8; 32] = entropy
        .try_into()
        .map_err(|_| "Recovery phrase did not produce 32 bytes of entropy")?;

    let data_dir = state.app_handle.path().app_data_dir()?;

    // Write the recovered master key.
    std::fs::write(data_dir.join("master_key.key"), master_key_bytes)?;

    // Reset derived key indices to 0 so setup re-derives from scratch.
    let _ = std::fs::remove_file(data_dir.join("signing_key.key"));
    let _ = std::fs::remove_file(data_dir.join("signing_key_index"));
    let _ = std::fs::remove_file(data_dir.join("dm_key_index"));
    let _ = std::fs::remove_file(data_dir.join("transport_key.key"));
    let _ = std::fs::remove_file(data_dir.join("device_index"));

    // The user obviously has the phrase -- mark it as backed up.
    std::fs::write(data_dir.join(".seed_backed_up"), b"1")?;

    // Wipe all social data so the app starts fresh with the recovered identity.
    state.storage.wipe_all_data().await?;

    // Rebuild the networking stack in place with the new identity.
    let state_arc: Arc<AppState> = Arc::clone(&state);
    crate::setup::rebuild_for_recovery(&state_arc).await?;

    Ok(())
}
