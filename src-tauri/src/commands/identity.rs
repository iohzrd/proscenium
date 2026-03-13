use crate::state::AppState;
use iroh::SecretKey;
use iroh_social_types::{
    SigningKeyDelegation, derive_signing_key, now_millis, sign_delegation, sign_rotation,
};
use std::sync::Arc;
use tauri::{Manager, State};

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_seed_phrase(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    state.get_seed_phrase().await
}

#[tauri::command]
pub async fn is_seed_phrase_backed_up(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    state.is_seed_phrase_backed_up().await
}

#[tauri::command]
pub async fn mark_seed_phrase_backed_up(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.mark_seed_phrase_backed_up().await
}

#[tauri::command]
pub async fn rotate_signing_key(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    state.rotate_signing_key().await
}

#[tauri::command]
pub async fn verify_seed_phrase_words(
    state: State<'_, Arc<AppState>>,
    checks: Vec<(usize, String)>,
) -> Result<bool, String> {
    state.verify_seed_phrase_words(checks).await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn get_seed_phrase(&self) -> Result<String, String> {
        let master_secret_key_bytes = self.identity.read().await.master_secret_key_bytes;
        let mnemonic = bip39::Mnemonic::from_entropy(&master_secret_key_bytes)
            .map_err(|e| format!("failed to generate mnemonic: {e}"))?;
        Ok(mnemonic.to_string())
    }

    pub(crate) async fn is_seed_phrase_backed_up(&self) -> Result<bool, String> {
        let data_dir = self
            .app_handle
            .path()
            .app_data_dir()
            .map_err(|e| format!("failed to resolve data dir: {e}"))?;
        Ok(data_dir.join(".seed_backed_up").exists())
    }

    pub(crate) async fn mark_seed_phrase_backed_up(&self) -> Result<(), String> {
        let data_dir = self
            .app_handle
            .path()
            .app_data_dir()
            .map_err(|e| format!("failed to resolve data dir: {e}"))?;
        tokio::fs::write(data_dir.join(".seed_backed_up"), b"1")
            .await
            .map_err(|e| format!("failed to write marker: {e}"))
    }

    pub(crate) async fn rotate_signing_key(&self) -> Result<String, String> {
        let data_dir = self
            .app_handle
            .path()
            .app_data_dir()
            .map_err(|e| format!("failed to resolve data dir: {e}"))?;

        let (
            old_index,
            master_secret_key_bytes,
            dm_pubkey,
            dm_key_index,
            master_pubkey,
            transport_node_id,
        ) = {
            let id = self.identity.read().await;
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

        self.gossip
            .broadcast_signing_key_rotation(&rotation)
            .await
            .map_err(|e| format!("failed to broadcast rotation: {e}"))?;

        let devices = self
            .storage
            .get_linked_devices()
            .await
            .map_err(|e| format!("failed to get devices: {e}"))?;
        let new_signing_sk = SecretKey::from_bytes(&new_signing_bytes);
        let current_version = self
            .storage
            .get_peer_announcement_version(&master_pubkey)
            .await
            .unwrap_or(None)
            .unwrap_or(devices.len() as u64);
        let version = current_version + 1;
        let mut announcement = iroh_social_types::LinkedDevicesAnnouncement {
            master_pubkey: master_pubkey.clone(),
            delegation: new_delegation.clone(),
            devices,
            version,
            timestamp: now,
            signature: String::new(),
        };
        iroh_social_types::sign_linked_devices_announcement(&mut announcement, &new_signing_sk);
        if let Err(e) = self
            .storage
            .cache_peer_device_announcement(&master_pubkey, &announcement)
            .await
        {
            log::error!("[rotate] failed to cache own announcement: {e}");
        }
        self.gossip
            .broadcast_linked_devices(&announcement)
            .await
            .map_err(|e| format!("failed to broadcast updated announcement: {e}"))?;

        let servers = self
            .storage
            .get_registered_servers()
            .await
            .map_err(|e| format!("failed to get servers: {e}"))?;
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
            let payload = iroh_social_types::RegistrationPayload {
                master_pubkey: master_pubkey.clone(),
                transport_node_id: transport_node_id.clone(),
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
                master_pubkey: master_pubkey.clone(),
                transport_node_id: transport_node_id.clone(),
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
            match self
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
        std::fs::write(&signing_key_index_path, new_index.to_string())
            .map_err(|e| format!("failed to write signing_key_index: {e}"))?;

        {
            let mut id = self.identity.write().await;
            id.signing_key_index = new_index;
            id.signing_secret_key_bytes = new_signing_bytes;
            id.signing_key = SecretKey::from_bytes(&new_signing_bytes);
            id.delegation = new_delegation;
        }

        log::info!("[rotate] signing key rotated from index {old_index} to {new_index}");
        Ok(format!("Signing key rotated to index {new_index}."))
    }

    pub(crate) async fn verify_seed_phrase_words(
        &self,
        checks: Vec<(usize, String)>,
    ) -> Result<bool, String> {
        let master_secret_key_bytes = self.identity.read().await.master_secret_key_bytes;
        let mnemonic = bip39::Mnemonic::from_entropy(&master_secret_key_bytes)
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
}
