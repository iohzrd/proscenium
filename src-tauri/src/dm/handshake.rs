use crate::crypto::{
    RatchetState, noise_complete_initiator, noise_complete_responder, noise_initiate, noise_respond,
};
use crate::error::AppError;
use iroh::EndpointId;
use proscenium_types::{DmHandshake, DmMessage, is_valid_dm_pubkey, short_id};

use super::crypto::dm_pubkey_to_x25519;
use super::{DmHandler, DmKeys, HANDSHAKE_RESPONSE_TIMEOUT, PeerRoute, SESSION_LOCK_TIMEOUT};

/// Byte length of Noise IK message 1 with an empty payload: the ephemeral key (32), the
/// encrypted static key (32 + 16) and the payload tag (16).
const NOISE_INIT_LEN: usize = 96;

/// Run the Noise IK handshake as initiator with the NodeIds of `peer`, in order, until
/// one answers, and store the resulting session. The caller holds the session lock of
/// the peer. The NodeId that answered is moved to the front of `peer.node_ids`.
pub(super) async fn establish_session(
    handler: &DmHandler,
    keys: &DmKeys,
    peer: &mut PeerRoute,
) -> Result<RatchetState, AppError> {
    let mut last_error = AppError::Other(format!(
        "no transport NodeId for {}",
        short_id(&peer.master_pubkey)
    ));
    for (index, node) in peer.node_ids.clone().into_iter().enumerate() {
        match handshake_with_node(handler, keys, peer, node).await {
            Ok(ratchet) => {
                handler
                    .save_session(keys, &peer.dm_pubkey, &ratchet)
                    .await?;
                peer.node_ids.swap(0, index);
                log::info!(
                    "[dm] established session with {}",
                    short_id(&peer.master_pubkey)
                );
                return Ok(ratchet);
            }
            Err(e) => {
                log::debug!(
                    "[dm] handshake with {} at node {} failed: {e}",
                    short_id(&peer.master_pubkey),
                    short_id(&node.to_string())
                );
                last_error = e;
            }
        }
    }
    Err(last_error)
}

/// Send `DmHandshake::Init` to `node`, read the response and derive the initiator
/// ratchet state. Nothing is stored.
async fn handshake_with_node(
    handler: &DmHandler,
    keys: &DmKeys,
    peer: &PeerRoute,
    node: EndpointId,
) -> Result<RatchetState, AppError> {
    let (initiator, noise_message) = noise_initiate(&keys.x25519_private, &peer.x25519_public)?;
    let frame = serde_json::to_vec(&DmMessage::Handshake(DmHandshake::Init {
        noise_message,
        sender: keys.dm_pubkey.clone(),
    }))?;

    let answer = handler
        .exchange(node, &frame, HANDSHAKE_RESPONSE_TIMEOUT)
        .await?;
    let DmHandshake::Response { noise_message } = serde_json::from_slice(&answer)? else {
        return Err(AppError::Other("unexpected handshake response".into()));
    };

    let seed = noise_complete_initiator(initiator, &noise_message)?;
    Ok(RatchetState::init_alice(&seed, &peer.x25519_public)?)
}

/// Handle `DmHandshake::Init` (Noise IK responder side) received from NodeId `remote`.
/// Returns the serialized `DmHandshake::Response`.
///
/// A session is stored only if all of these hold: `sender` is the static key the Noise
/// handshake authenticated; `sender` is the DM pubkey of a cached delegation, or of the
/// verified identity that `remote` returns; that user is not blocked.
pub(super) async fn handle_handshake(
    handler: &DmHandler,
    remote: EndpointId,
    sender: &str,
    noise_message: &[u8],
) -> Result<Vec<u8>, AppError> {
    if !is_valid_dm_pubkey(sender) {
        return Err(AppError::Other(
            "handshake sender is not a DM pubkey".into(),
        ));
    }
    if noise_message.len() != NOISE_INIT_LEN {
        return Err(AppError::Other(format!(
            "handshake message has {} bytes, expected {NOISE_INIT_LEN}",
            noise_message.len()
        )));
    }
    let keys = handler.dm_keys().await;
    if sender == keys.dm_pubkey {
        return Err(AppError::Other(
            "handshake sender is the local DM pubkey".into(),
        ));
    }
    let claimed_key = dm_pubkey_to_x25519(sender)?;

    // Noise IK responder. No state is written before the initiator's key is authenticated.
    let (responder, response_msg) = noise_respond(&keys.x25519_private, noise_message)?;
    let (seed, authenticated_key) = noise_complete_responder(responder)?;
    if authenticated_key != claimed_key {
        return Err(AppError::Other(
            "DmHandshake::Init sender mismatch: claimed key does not match authenticated key"
                .into(),
        ));
    }

    let peer_master_pubkey = match handler
        .storage
        .get_master_pubkey_for_dm_pubkey(sender)
        .await
    {
        Some(master_pubkey) => {
            if handler.storage.is_blocked(&master_pubkey).await? {
                return Err(AppError::Other("peer is blocked".into()));
            }
            master_pubkey
        }
        None => {
            // Unknown DM key: ask the connecting node for its signed identity.
            let identity = crate::peer::query_identity(&handler.endpoint, remote).await?;
            if identity.delegation().dm_pubkey != sender {
                return Err(AppError::Other(
                    "handshake sender is not the DM pubkey of the node's identity".into(),
                ));
            }
            if identity.master_pubkey == keys.master_pubkey {
                return Err(AppError::Other("handshake from the local identity".into()));
            }
            if handler.storage.is_blocked(&identity.master_pubkey).await? {
                return Err(AppError::Other("peer is blocked".into()));
            }
            handler.storage.cache_peer_identity(&identity).await?;
            identity.master_pubkey
        }
    };

    // Initialize Double Ratchet as Bob (responder). This replaces any stored session
    // with the peer.
    let ratchet = RatchetState::init_bob(&seed, (*keys.x25519_private, keys.x25519_public));
    {
        let _session_guard =
            tokio::time::timeout(SESSION_LOCK_TIMEOUT, handler.session_locks.lock(sender))
                .await
                .map_err(|_| AppError::Other("session is in use".into()))?;
        handler.save_session(&keys, sender, &ratchet).await?;
    }

    log::info!(
        "[dm] session established by {}",
        short_id(&peer_master_pubkey)
    );

    let resp = DmHandshake::Response {
        noise_message: response_msg,
    };
    Ok(serde_json::to_vec(&resp)?)
}
