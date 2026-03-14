use crate::crypto::{
    ed25519_secret_to_x25519, noise_psk_complete_initiator, noise_psk_initiate,
    noise_transport_decrypt,
};
use crate::error::CmdResult;
use crate::state::AppState;
use iroh_social_types::{
    DeviceEntry, LinkBundleData, LinkQrPayload, PEER_ALPN, PeerRequest, PeerResponse,
    derive_transport_key,
};
use std::sync::Arc;
use tauri::{Manager, State};

#[tauri::command]
pub async fn start_device_link(
    state: State<'_, Arc<AppState>>,
    transfer_master_key: bool,
) -> CmdResult<LinkQrPayload> {
    let relay_url = state
        .endpoint
        .addr()
        .relay_urls()
        .next()
        .map(|u| u.to_string());
    state
        .peer
        .start_link_session(transfer_master_key, relay_url)
        .await
}

#[tauri::command]
pub async fn cancel_device_link(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.peer.cancel_link_session().await;
    Ok(())
}

#[tauri::command]
pub async fn link_with_device(
    state: State<'_, Arc<AppState>>,
    qr_payload: LinkQrPayload,
) -> CmdResult<()> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let psk_bytes = b64.decode(&qr_payload.secret)?;
    if psk_bytes.len() != 32 {
        return Err("invalid PSK length".into());
    }
    let mut psk = [0u8; 32];
    psk.copy_from_slice(&psk_bytes);

    let target: iroh::EndpointId = qr_payload.node_id.parse()?;

    let (master_secret_key_bytes, master_pubkey) = {
        let id = state.identity.read().await;
        (id.master_secret_key_bytes, id.master_pubkey.clone())
    };

    let transport_secret_bytes = derive_transport_key(&master_secret_key_bytes, 0);
    let my_x25519_private = ed25519_secret_to_x25519(&transport_secret_bytes);
    let peer_x25519_public = crate::crypto::ed25519_public_to_x25519(target.as_bytes())
        .ok_or("invalid peer public key")?;

    let (initiator_hs, noise_init) =
        noise_psk_initiate(&my_x25519_private, &peer_x25519_public, &psk)?;

    let addr = iroh::EndpointAddr::from(target);
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        state.endpoint.connect(addr, PEER_ALPN),
    )
    .await
    .map_err(|_| "connection timeout")??;

    let (mut send, mut recv) = conn.open_bi().await?;

    let req = PeerRequest::LinkRequest { noise_init };
    let req_bytes = serde_json::to_vec(&req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    let resp_bytes = recv.read_to_end(10_000_000).await?;
    let response: PeerResponse = serde_json::from_slice(&resp_bytes)?;

    conn.close(0u32.into(), b"done");

    let (noise_response, encrypted_bundle) = match response {
        PeerResponse::LinkBundle {
            noise_response,
            encrypted_bundle,
        } => (noise_response, encrypted_bundle),
        _ => return Err("unexpected response type".into()),
    };

    let mut transport = noise_psk_complete_initiator(initiator_hs, &noise_response)?;
    let bundle_json = noise_transport_decrypt(&mut transport, &encrypted_bundle)?;

    let bundle: LinkBundleData = serde_json::from_slice(&bundle_json)?;

    log::info!("[link] received link bundle, importing data...");

    state
        .storage
        .import_link_bundle(&master_pubkey, &bundle)
        .await?;

    let data_dir = state.app_handle.path().app_data_dir()?;

    let signing_key_bytes = b64.decode(&bundle.signing_secret_key)?;
    tokio::fs::write(data_dir.join("signing_key.key"), &signing_key_bytes).await?;

    tokio::fs::write(
        data_dir.join("signing_key_index"),
        bundle.delegation.key_index.to_string(),
    )
    .await?;

    tokio::fs::write(
        data_dir.join("dm_key_index"),
        bundle.delegation.dm_key_index.to_string(),
    )
    .await?;

    let transport_key_bytes = b64.decode(&bundle.transport_secret_key)?;
    tokio::fs::write(data_dir.join("transport_key.key"), &transport_key_bytes).await?;

    tokio::fs::write(
        data_dir.join("device_index"),
        bundle.device_index.to_string(),
    )
    .await?;

    if let Some(ref master_key_b64) = bundle.master_secret_key {
        let master_bytes = b64.decode(master_key_b64)?;
        tokio::fs::write(data_dir.join("master_key.key"), &master_bytes).await?;
    }

    log::info!(
        "[link] device linked successfully (device_index={})",
        bundle.device_index
    );
    Ok(())
}

#[tauri::command]
pub async fn get_linked_devices(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<DeviceEntry>> {
    state.storage.get_linked_devices().await
}

#[tauri::command]
pub async fn force_device_sync(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    let (master_pubkey, signing_secret_key_bytes) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.signing_secret_key_bytes)
    };
    crate::device_sync::sync_all_devices(
        &state.endpoint,
        &state.storage,
        &master_pubkey,
        &signing_secret_key_bytes,
    )
    .await;
    Ok(())
}
