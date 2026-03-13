use crate::crypto::{
    ed25519_secret_to_x25519, noise_psk_complete_initiator, noise_psk_initiate,
    noise_transport_decrypt,
};
use crate::state::AppState;
use iroh_social_types::{
    DeviceEntry, LinkBundleData, LinkQrPayload, PEER_ALPN, PeerRequest, PeerResponse,
    derive_transport_key,
};
use std::sync::Arc;
use tauri::{Manager, State};

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_device_link(
    state: State<'_, Arc<AppState>>,
    transfer_master_key: bool,
) -> Result<LinkQrPayload, String> {
    state.start_device_link(transfer_master_key).await
}

#[tauri::command]
pub async fn cancel_device_link(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.cancel_device_link().await
}

#[tauri::command]
pub async fn link_with_device(
    state: State<'_, Arc<AppState>>,
    qr_payload: LinkQrPayload,
) -> Result<(), String> {
    state.link_with_device(qr_payload).await
}

#[tauri::command]
pub async fn get_linked_devices(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<DeviceEntry>, String> {
    state.get_linked_devices().await
}

#[tauri::command]
pub async fn force_device_sync(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.force_device_sync().await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn start_device_link(
        &self,
        transfer_master_key: bool,
    ) -> Result<LinkQrPayload, String> {
        let relay_url = self
            .endpoint
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string());
        self.peer
            .start_link_session(transfer_master_key, relay_url)
            .await
    }

    pub(crate) async fn cancel_device_link(&self) -> Result<(), String> {
        self.peer.cancel_link_session().await;
        Ok(())
    }

    pub(crate) async fn link_with_device(&self, qr_payload: LinkQrPayload) -> Result<(), String> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;

        let psk_bytes = b64
            .decode(&qr_payload.secret)
            .map_err(|e| format!("invalid PSK encoding: {e}"))?;
        if psk_bytes.len() != 32 {
            return Err("invalid PSK length".to_string());
        }
        let mut psk = [0u8; 32];
        psk.copy_from_slice(&psk_bytes);

        let target: iroh::EndpointId = qr_payload
            .node_id
            .parse()
            .map_err(|e| format!("invalid node_id: {e}"))?;

        let (master_secret_key_bytes, master_pubkey) = {
            let id = self.identity.read().await;
            (id.master_secret_key_bytes, id.master_pubkey.clone())
        };

        let transport_secret_bytes = derive_transport_key(&master_secret_key_bytes, 0);
        let my_x25519_private = ed25519_secret_to_x25519(&transport_secret_bytes);
        let peer_x25519_public = crate::crypto::ed25519_public_to_x25519(target.as_bytes())
            .ok_or("invalid peer public key")?;

        let (initiator_hs, noise_init) =
            noise_psk_initiate(&my_x25519_private, &peer_x25519_public, &psk)
                .map_err(|e| format!("noise init failed: {e}"))?;

        let addr = iroh::EndpointAddr::from(target);
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            self.endpoint.connect(addr, PEER_ALPN),
        )
        .await
        .map_err(|_| "connection timeout")?
        .map_err(|e| format!("connection failed: {e}"))?;

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| format!("failed to open bi-stream: {e}"))?;

        let req = PeerRequest::LinkRequest { noise_init };
        let req_bytes = serde_json::to_vec(&req).map_err(|e| format!("serialize failed: {e}"))?;
        send.write_all(&req_bytes)
            .await
            .map_err(|e| format!("send failed: {e}"))?;
        send.finish().map_err(|e| format!("finish failed: {e}"))?;

        let resp_bytes = recv
            .read_to_end(10_000_000)
            .await
            .map_err(|e| format!("recv failed: {e}"))?;
        let response: PeerResponse =
            serde_json::from_slice(&resp_bytes).map_err(|e| format!("deserialize failed: {e}"))?;

        conn.close(0u32.into(), b"done");

        let (noise_response, encrypted_bundle) = match response {
            PeerResponse::LinkBundle {
                noise_response,
                encrypted_bundle,
            } => (noise_response, encrypted_bundle),
            _ => return Err("unexpected response type".to_string()),
        };

        let mut transport = noise_psk_complete_initiator(initiator_hs, &noise_response)
            .map_err(|e| format!("noise complete failed: {e}"))?;
        let bundle_json = noise_transport_decrypt(&mut transport, &encrypted_bundle)
            .map_err(|e| format!("decrypt failed (wrong QR code?): {e}"))?;

        let bundle: LinkBundleData = serde_json::from_slice(&bundle_json)
            .map_err(|e| format!("bundle parse failed: {e}"))?;

        log::info!("[link] received link bundle, importing data...");

        self.storage
            .import_link_bundle(&master_pubkey, &bundle)
            .await
            .map_err(|e| format!("import failed: {e}"))?;

        let data_dir = self
            .app_handle
            .path()
            .app_data_dir()
            .map_err(|e| format!("failed to resolve data dir: {e}"))?;

        let signing_key_bytes = b64
            .decode(&bundle.signing_secret_key)
            .map_err(|e| format!("invalid signing key: {e}"))?;
        tokio::fs::write(data_dir.join("signing_key.key"), &signing_key_bytes)
            .await
            .map_err(|e| format!("failed to save signing key: {e}"))?;

        tokio::fs::write(
            data_dir.join("signing_key_index"),
            bundle.delegation.key_index.to_string(),
        )
        .await
        .map_err(|e| format!("failed to save signing_key_index: {e}"))?;

        tokio::fs::write(
            data_dir.join("dm_key_index"),
            bundle.delegation.dm_key_index.to_string(),
        )
        .await
        .map_err(|e| format!("failed to save dm_key_index: {e}"))?;

        let transport_key_bytes = b64
            .decode(&bundle.transport_secret_key)
            .map_err(|e| format!("invalid transport key: {e}"))?;
        tokio::fs::write(data_dir.join("transport_key.key"), &transport_key_bytes)
            .await
            .map_err(|e| format!("failed to save transport key: {e}"))?;

        tokio::fs::write(
            data_dir.join("device_index"),
            bundle.device_index.to_string(),
        )
        .await
        .map_err(|e| format!("failed to save device index: {e}"))?;

        if let Some(ref master_key_b64) = bundle.master_secret_key {
            let master_bytes = b64
                .decode(master_key_b64)
                .map_err(|e| format!("invalid master key: {e}"))?;
            tokio::fs::write(data_dir.join("master_key.key"), &master_bytes)
                .await
                .map_err(|e| format!("failed to save master key: {e}"))?;
        }

        log::info!(
            "[link] device linked successfully (device_index={})",
            bundle.device_index
        );
        Ok(())
    }

    pub(crate) async fn get_linked_devices(&self) -> Result<Vec<DeviceEntry>, String> {
        self.storage
            .get_linked_devices()
            .await
            .map_err(|e| format!("failed to get linked devices: {e}"))
    }

    pub(crate) async fn force_device_sync(&self) -> Result<(), String> {
        let (master_pubkey, signing_secret_key_bytes) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.signing_secret_key_bytes)
        };
        crate::device_sync::sync_all_devices(
            &self.endpoint,
            &self.storage,
            &master_pubkey,
            &signing_secret_key_bytes,
        )
        .await;
        Ok(())
    }
}
