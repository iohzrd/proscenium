use crate::crypto::{
    ed25519_secret_to_x25519, noise_psk_complete_initiator, noise_psk_initiate,
    noise_transport_decrypt,
};
use crate::state::{AppState, PendingLink};
use iroh_social_types::{
    DeviceEntry, LinkBundleData, LinkQrPayload, PEER_ALPN, PeerRequest, PeerResponse,
    derive_transport_key, now_millis,
};
use std::sync::Arc;
use tauri::{Manager, State};

const LINK_SESSION_TTL_MS: u64 = 60_000; // 60 seconds

/// Generate a QR payload and start listening for a new device to pair.
/// The existing device calls this to begin the pairing flow.
#[tauri::command]
pub async fn start_device_link(
    state: State<'_, Arc<AppState>>,
    transfer_master_key: bool,
) -> Result<LinkQrPayload, String> {
    // Generate a one-time PSK
    let mut psk = [0u8; 32];
    getrandom::fill(&mut psk).map_err(|e| format!("failed to generate PSK: {e}"))?;

    // Derive X25519 key from the transport Ed25519 key for the Noise handshake
    let transport_secret_bytes = derive_transport_key(&state.identity.master_secret_key_bytes, 0);
    let x25519_private = ed25519_secret_to_x25519(&transport_secret_bytes);

    let expires_at = now_millis() + LINK_SESSION_TTL_MS;

    // Store the pending link session
    {
        let mut lock = state.pending_link.lock().await;
        *lock = Some(PendingLink {
            psk,
            x25519_private,
            expires_at,
            transfer_master_key,
        });
    }

    // Build the QR payload
    let relay_url = state
        .net
        .endpoint
        .addr()
        .relay_urls()
        .next()
        .map(|u| u.to_string());

    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let payload = LinkQrPayload {
        node_id: state.identity.transport_node_id.clone(),
        secret: b64.encode(psk),
        relay_url,
    };

    log::info!("[link] started device link session (expires in 60s)");
    Ok(payload)
}

/// Cancel an active device link session.
#[tauri::command]
pub async fn cancel_device_link(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut lock = state.pending_link.lock().await;
    *lock = None;
    log::info!("[link] cancelled device link session");
    Ok(())
}

/// New device: connect to the existing device using the QR payload, receive the link bundle,
/// and import all data.
#[tauri::command]
pub async fn link_with_device(
    app_handle: tauri::AppHandle,
    qr_payload: LinkQrPayload,
) -> Result<(), String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;

    // Decode the PSK from the QR payload
    let psk_bytes = b64
        .decode(&qr_payload.secret)
        .map_err(|e| format!("invalid PSK encoding: {e}"))?;
    if psk_bytes.len() != 32 {
        return Err("invalid PSK length".to_string());
    }
    let mut psk = [0u8; 32];
    psk.copy_from_slice(&psk_bytes);

    // Get the state
    let state = app_handle
        .try_state::<Arc<AppState>>()
        .ok_or("app state not ready")?;

    // Parse the existing device's NodeId
    let target: iroh::EndpointId = qr_payload
        .node_id
        .parse()
        .map_err(|e| format!("invalid node_id: {e}"))?;

    // Derive our X25519 key for the Noise handshake
    let transport_secret_bytes = derive_transport_key(&state.identity.master_secret_key_bytes, 0);
    let my_x25519_private = ed25519_secret_to_x25519(&transport_secret_bytes);

    // Derive the peer's X25519 public key from their Ed25519 NodeId
    let peer_x25519_public = crate::crypto::ed25519_public_to_x25519(target.as_bytes())
        .ok_or("invalid peer public key")?;

    // Create the Noise IK+PSK initiator handshake
    let (initiator_hs, noise_init) =
        noise_psk_initiate(&my_x25519_private, &peer_x25519_public, &psk)
            .map_err(|e| format!("noise init failed: {e}"))?;

    // Connect to the existing device
    let addr = iroh::EndpointAddr::from(target);
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        state.net.endpoint.connect(addr, PEER_ALPN),
    )
    .await
    .map_err(|_| "connection timeout")?
    .map_err(|e| format!("connection failed: {e}"))?;

    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| format!("failed to open bi-stream: {e}"))?;

    // Send LinkRequest
    let req = PeerRequest::LinkRequest { noise_init };
    let req_bytes = serde_json::to_vec(&req).map_err(|e| format!("serialize failed: {e}"))?;
    send.write_all(&req_bytes)
        .await
        .map_err(|e| format!("send failed: {e}"))?;
    send.finish().map_err(|e| format!("finish failed: {e}"))?;

    // Read response
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

    // Complete the Noise handshake
    let mut transport = noise_psk_complete_initiator(initiator_hs, &noise_response)
        .map_err(|e| format!("noise complete failed: {e}"))?;

    // Decrypt the bundle
    let bundle_json = noise_transport_decrypt(&mut transport, &encrypted_bundle)
        .map_err(|e| format!("decrypt failed (wrong QR code?): {e}"))?;

    let bundle: LinkBundleData =
        serde_json::from_slice(&bundle_json).map_err(|e| format!("bundle parse failed: {e}"))?;

    log::info!("[link] received link bundle, importing data...");

    // Import the bundle into storage
    state
        .storage
        .import_link_bundle(&state.identity.master_pubkey, &bundle)
        .await
        .map_err(|e| format!("import failed: {e}"))?;

    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve data dir: {e}"))?;

    // Save the signing key
    let signing_key_bytes = b64
        .decode(&bundle.signing_secret_key)
        .map_err(|e| format!("invalid signing key: {e}"))?;
    tokio::fs::write(data_dir.join("signing_key.key"), &signing_key_bytes)
        .await
        .map_err(|e| format!("failed to save signing key: {e}"))?;

    // Save the signing key index so setup.rs re-derives the correct key on restart
    tokio::fs::write(
        data_dir.join("signing_key_index"),
        bundle.delegation.key_index.to_string(),
    )
    .await
    .map_err(|e| format!("failed to save signing_key_index: {e}"))?;

    // Save the DM key index so setup.rs re-derives the correct DM key on restart
    tokio::fs::write(
        data_dir.join("dm_key_index"),
        bundle.delegation.dm_key_index.to_string(),
    )
    .await
    .map_err(|e| format!("failed to save dm_key_index: {e}"))?;

    // Save the transport key for this device
    let transport_key_bytes = b64
        .decode(&bundle.transport_secret_key)
        .map_err(|e| format!("invalid transport key: {e}"))?;
    tokio::fs::write(data_dir.join("transport_key.key"), &transport_key_bytes)
        .await
        .map_err(|e| format!("failed to save transport key: {e}"))?;

    // Save device index
    tokio::fs::write(
        data_dir.join("device_index"),
        bundle.device_index.to_string(),
    )
    .await
    .map_err(|e| format!("failed to save device index: {e}"))?;

    // Optionally save the master key
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

/// Get all linked devices.
#[tauri::command]
pub async fn get_linked_devices(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<DeviceEntry>, String> {
    state
        .storage
        .get_linked_devices()
        .await
        .map_err(|e| format!("failed to get linked devices: {e}"))
}

/// Force an immediate device sync with all linked devices.
#[tauri::command]
pub async fn force_device_sync(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    crate::device_sync::sync_all_devices(
        &state.net.endpoint,
        &state.storage,
        &state.identity.master_pubkey,
        &state.identity.signing_secret_key_bytes,
    )
    .await;
    Ok(())
}
