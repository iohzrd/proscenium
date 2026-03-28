use crate::crypto::{
    RatchetState, noise_complete_initiator, noise_complete_responder, noise_initiate, noise_respond,
};
use crate::error::AppError;
use iroh::{EndpointAddr, EndpointId};
use proscenium_types::{DM_ALPN, DmHandshake, DmMessage, now_millis, short_id};

use super::DmHandler;
use super::crypto::{dm_pubkey_to_x25519, open_ratchet_state, seal_ratchet_state};

/// Get or establish a ratchet session with a peer.
/// Returns `(RatchetState, peer_dm_pubkey)` -- callers must use `peer_dm_pubkey`
/// as the key for any subsequent `save_ratchet_session` calls.
pub(super) async fn get_or_establish_session(
    handler: &DmHandler,
    peer_master_pubkey: &str,
) -> Result<(RatchetState, String), AppError> {
    log::info!(
        "[dm] get_or_establish_session: peer={}",
        short_id(peer_master_pubkey)
    );

    // Resolve the peer's DM pubkey from cached delegation.
    let peer_dm_pubkey = handler
        .storage
        .get_peer_dm_pubkey(peer_master_pubkey)
        .await?
        .ok_or_else(|| {
            AppError::Other(format!(
                "no DM pubkey cached for {}",
                short_id(peer_master_pubkey)
            ))
        })?;

    // Read all identity fields needed before any await points.
    let (my_x25519_private, my_dm_pubkey, ratchet_storage_key) = {
        let id = handler.identity.read().await;
        (
            id.dm_x25519_private,
            id.dm_pubkey.clone(),
            id.ratchet_storage_key,
        )
    };

    // Try loading existing session (keyed by DM pubkey)
    if let Some(stored) = handler.storage.get_ratchet_session(&peer_dm_pubkey).await? {
        log::info!(
            "[dm] loaded existing ratchet session for {}",
            short_id(peer_master_pubkey),
        );
        let json = open_ratchet_state(&ratchet_storage_key, &stored)?;
        let state: RatchetState = serde_json::from_str(&json)?;
        return Ok((state, peer_dm_pubkey));
    }

    // No session -- need to handshake
    log::info!(
        "[dm] no existing session, initiating Noise IK handshake with {}",
        short_id(peer_master_pubkey)
    );

    // Fail fast: resolve transport NodeId before doing any crypto work
    let node_ids = handler
        .storage
        .get_peer_transport_node_ids(peer_master_pubkey)
        .await?;
    let transport_id_str = node_ids.into_iter().next().ok_or_else(|| {
        AppError::Other(format!(
            "no transport NodeId for {}",
            short_id(peer_master_pubkey)
        ))
    })?;
    let transport_id: EndpointId = transport_id_str.parse()?;
    let addr = EndpointAddr::from(transport_id);

    // Peer's DM pubkey IS already X25519 -- just hex-decode it
    let peer_x25519_public = dm_pubkey_to_x25519(&peer_dm_pubkey)?;
    log::info!("[dm] resolved peer DM X25519 key");

    // Noise IK handshake: initiator
    let (initiator_hs, msg1) = noise_initiate(&my_x25519_private, &peer_x25519_public)?;
    log::info!("[dm] noise init message created ({} bytes)", msg1.len());

    log::info!(
        "[dm] connecting to {} on DM_ALPN...",
        short_id(peer_master_pubkey)
    );
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        handler.endpoint.connect(addr, DM_ALPN),
    )
    .await
    .map_err(|_| {
        AppError::Other(format!(
            "handshake connect timeout to {}",
            short_id(peer_master_pubkey)
        ))
    })?
    .inspect_err(|e| {
        log::error!(
            "[dm] QUIC connect failed to {}: {e}",
            short_id(peer_master_pubkey)
        );
    })?;
    log::info!("[dm] QUIC connected, opening bi-stream...");
    let (mut send, mut recv) = conn.open_bi().await?;

    // Send handshake init -- sender is our DM pubkey
    let handshake = DmHandshake::Init {
        noise_message: msg1,
        sender: my_dm_pubkey,
    };
    let bytes = serde_json::to_vec(&DmMessage::Handshake(handshake))?;
    log::info!("[dm] sending handshake init ({} bytes)...", bytes.len());
    send.write_all(&bytes).await?;
    send.finish()?;

    // Read handshake response (acceptor sends DmHandshake::Response directly)
    log::info!("[dm] waiting for handshake response...");
    let resp_bytes = recv.read_to_end(65536).await?;
    log::info!(
        "[dm] received handshake response ({} bytes)",
        resp_bytes.len()
    );
    let resp: DmHandshake = serde_json::from_slice(&resp_bytes)?;

    let noise_response = match resp {
        DmHandshake::Response { noise_message } => noise_message,
        _ => return Err(AppError::Other("unexpected handshake response".into())),
    };

    // Complete handshake
    let shared_secret = noise_complete_initiator(initiator_hs, &noise_response)?;
    log::info!("[dm] noise handshake completed successfully");

    conn.close(0u32.into(), b"done");

    // Initialize Double Ratchet as Alice (initiator)
    let ratchet = RatchetState::init_alice(&shared_secret, &peer_x25519_public);

    // Encrypt and save session keyed by peer's DM pubkey
    let json = serde_json::to_string(&ratchet)?;
    let sealed = seal_ratchet_state(&ratchet_storage_key, &json)?;
    handler
        .storage
        .save_ratchet_session(&peer_dm_pubkey, &sealed, now_millis())
        .await?;

    log::info!(
        "[dm] established and saved ratchet session with {}",
        short_id(peer_master_pubkey)
    );
    Ok((ratchet, peer_dm_pubkey))
}

/// Handle an incoming handshake (Noise IK responder side).
/// `peer_dm_pubkey` is taken from `DmHandshake::Init.sender`.
pub(super) async fn handle_handshake(
    handler: &DmHandler,
    peer_dm_pubkey: &str,
    noise_message: Vec<u8>,
) -> Result<Vec<u8>, AppError> {
    log::info!("[dm] handling handshake from {}", short_id(peer_dm_pubkey));

    let (my_x25519_private, my_x25519_public, ratchet_storage_key) = {
        let id = handler.identity.read().await;
        (
            id.dm_x25519_private,
            id.dm_x25519_public,
            id.ratchet_storage_key,
        )
    };

    // Noise IK responder
    let (responder_hs, response_msg) = noise_respond(&my_x25519_private, &noise_message)?;

    let (shared_secret, initiator_dm_pubkey) = noise_complete_responder(responder_hs)?;

    // Verify the Noise IK-authenticated initiator DM key matches the claimed sender.
    let claimed_key = dm_pubkey_to_x25519(peer_dm_pubkey)?;
    match initiator_dm_pubkey {
        Some(actual) if actual == claimed_key => {}
        Some(_) => {
            return Err(AppError::Other(
                "DmHandshake::Init sender mismatch: claimed key does not match authenticated key"
                    .into(),
            ));
        }
        None => {
            return Err(AppError::Other(
                "Noise IK handshake did not reveal initiator key".into(),
            ));
        }
    }

    // Initialize Double Ratchet as Bob (responder)
    let ratchet = RatchetState::init_bob(&shared_secret, (my_x25519_private, my_x25519_public));

    // Save session keyed by peer's DM pubkey
    let json = serde_json::to_string(&ratchet)?;
    let sealed = seal_ratchet_state(&ratchet_storage_key, &json)?;
    handler
        .storage
        .save_ratchet_session(peer_dm_pubkey, &sealed, now_millis())
        .await?;

    log::info!("[dm] session established with {}", short_id(peer_dm_pubkey));

    let resp = DmHandshake::Response {
        noise_message: response_msg,
    };
    Ok(serde_json::to_vec(&resp)?)
}
