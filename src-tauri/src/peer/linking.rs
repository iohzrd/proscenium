//! Device linking over the peer ALPN.
//!
//! The existing device shows a QR code with its NodeId and a one-time PSK. The new device
//! connects and both run a Noise IKpsk2 handshake.
//!
//! Stream 1 (opened by the new device):
//! - new device: JSON `PeerRequest::LinkRequest { noise_init }`, then finish.
//! - existing device: chunks (`[u32 big-endian length][bytes]`): the JSON
//!   `PeerResponse::LinkBundle { noise_response }`, then the `LinkBundleData` JSON as Noise
//!   transport messages, then a zero-length chunk.
//!
//! Stream 2 (opened by the new device), the key confirmation:
//! - new device: `LINK_CONFIRM` as one Noise transport message, then finish.
//! - existing device: `LINK_ACK` as one Noise transport message, then finish.
//!
//! The new device then closes the connection with `LINK_CLOSE_DONE`. The first IKpsk2
//! message does not depend on the PSK, so the existing device treats the request as
//! authenticated only after it decrypted `LINK_CONFIRM`: only then is the session removed.
//! It stores the new device only after it received `LINK_CLOSE_DONE`.

use crate::crypto::{
    ed25519_public_to_x25519, ed25519_secret_to_x25519, noise_psk_complete_initiator,
    noise_psk_initiate, noise_psk_respond, noise_transport_decrypt, noise_transport_encrypt,
};
use crate::error::AppError;
use crate::setup::IdentityKeys;
use crate::state::{device_index_of, device_node_id, own_devices};
use base64::Engine;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayUrl,
    endpoint::{Connection, ConnectionError, RecvStream, SendStream, VarInt},
    protocol::AcceptError,
};
use proscenium_types::{
    LinkBundleData, LinkQrPayload, MAX_LINKED_DEVICES, PEER_ALPN, PeerRequest, PeerResponse,
    derive_transport_key, now_millis, short_id,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use zeroize::Zeroize;

use super::PeerHandler;

/// Device-link session on the existing device, created by `start_link_session`.
///
/// It is removed when a new device completes the key confirmation, when a request
/// arrives after it expired, when it is cancelled or replaced, or by the request that
/// follows `MAX_LINK_ATTEMPTS` attempts.
pub(super) struct PendingLink {
    psk: [u8; 32],
    expires_at: u64,
    attempts: u32,
}

pub(super) type PendingLinkState = Arc<tokio::sync::Mutex<Option<PendingLink>>>;

pub(super) const LINK_SESSION_TTL_MS: u64 = 60_000; // 60 seconds
/// Link requests served per session. A request with a wrong PSK counts as one.
const MAX_LINK_ATTEMPTS: u32 = 5;
/// Time limit for each network step of the link exchange.
const LINK_STEP_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest Noise message in bytes (snow `MAXMSGLEN`).
const NOISE_MAX_MESSAGE_LEN: usize = 65_535;
/// Length of the AEAD tag that each Noise transport message carries.
const NOISE_TAG_LEN: usize = 16;
/// Largest accepted `PeerResponse::LinkBundle` JSON.
const MAX_LINK_RESPONSE_LEN: usize = 4_096;
/// Largest accepted `LinkBundleData` JSON.
const MAX_BUNDLE_LEN: usize = 16 * 1024 * 1024;
/// Largest accepted key confirmation or acknowledgement message.
const MAX_CONFIRM_LEN: usize = 256;

/// Plaintext of the key confirmation sent by the new device.
const LINK_CONFIRM: &[u8] = b"proscenium/link-confirm/2";
/// Plaintext of the acknowledgement sent by the existing device.
const LINK_ACK: &[u8] = b"proscenium/link-ack/2";
/// QUIC close code sent by the new device after it verified `LINK_ACK`.
/// Not 0: dropping a connection without `close` sends code 0.
const LINK_CLOSE_DONE: u32 = 1;
/// QUIC close code sent by either device when the exchange failed.
const LINK_CLOSE_FAILED: u32 = 2;

/// Count one link attempt against the session in `slot` and return its PSK.
/// Removes the session if it expired or already served `MAX_LINK_ATTEMPTS` requests.
fn begin_link_attempt(slot: &mut Option<PendingLink>, now: u64) -> Result<[u8; 32], AppError> {
    let Some(session) = slot.as_mut() else {
        return Err("no pending link session".into());
    };
    let expired = now > session.expires_at;
    let exhausted = session.attempts >= MAX_LINK_ATTEMPTS;
    if expired || exhausted {
        *slot = None;
        return Err(if expired {
            "link session expired".into()
        } else {
            "too many link attempts for this session".into()
        });
    }
    session.attempts += 1;
    Ok(session.psk)
}

/// Remove the session in `slot` if it holds `psk`. Returns `false` if the session was
/// cancelled, replaced, or removed by another request in the meantime.
fn consume_link_session(slot: &mut Option<PendingLink>, psk: &[u8; 32]) -> bool {
    let matches = slot.as_ref().is_some_and(|session| session.psk == *psk);
    if matches {
        *slot = None;
    }
    matches
}

/// Lowest transport key derivation index in `1..MAX_LINKED_DEVICES` whose NodeId is not
/// in `used_node_ids`. `None` if every index is in use.
fn lowest_unused_device_index(
    master_secret_key_bytes: &[u8; 32],
    used_node_ids: &HashSet<String>,
) -> Option<u32> {
    (1..MAX_LINKED_DEVICES as u32)
        .find(|index| !used_node_ids.contains(&device_node_id(master_secret_key_bytes, *index)))
}

/// Decoded fields of a `LinkQrPayload`.
struct LinkTarget {
    node_id: EndpointId,
    psk: [u8; 32],
    addr: EndpointAddr,
}

/// Decode and check a `LinkQrPayload`: the NodeId parses, the PSK is 32 bytes, and
/// `expires_at` is later than `now`. A relay URL that does not parse is ignored.
fn parse_link_payload(payload: &LinkQrPayload, now: u64) -> Result<LinkTarget, AppError> {
    let node_id: EndpointId = payload
        .node_id
        .parse()
        .map_err(|_| AppError::Other("link code holds an invalid NodeId".into()))?;
    let mut decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&payload.secret)
        .map_err(|_| AppError::Other("link code holds an invalid secret".into()))?;
    let psk: Result<[u8; 32], _> = decoded.as_slice().try_into();
    decoded.zeroize();
    let psk = psk.map_err(|_| AppError::Other("link code secret is not 32 bytes".into()))?;
    if now >= payload.expires_at {
        return Err("link code expired".into());
    }
    let mut addr = EndpointAddr::new(node_id);
    if let Some(relay_url) = payload
        .relay_url
        .as_deref()
        .and_then(|url| url.parse::<RelayUrl>().ok())
    {
        addr = addr.with_relay_url(relay_url);
    }
    Ok(LinkTarget { node_id, psk, addr })
}

/// Check a decrypted link bundle received from the device `responder_node_id` and return
/// the keys of the identity it defines.
///
/// Fails unless: the master key is 32 bytes, `device_index` is in
/// `1..MAX_LINKED_DEVICES`, the responder's NodeId derives from the master key at another
/// index, and the device list does not already hold the assigned NodeId.
/// `bundle.devices` is reduced to the entries whose NodeId derives from the master key,
/// and `bundle.master_secret_key` is cleared.
fn validate_link_bundle(
    bundle: &mut LinkBundleData,
    responder_node_id: &str,
) -> Result<IdentityKeys, AppError> {
    let decoded =
        base64::engine::general_purpose::STANDARD.decode(bundle.master_secret_key.as_bytes());
    bundle.master_secret_key.zeroize();
    let mut decoded =
        decoded.map_err(|_| AppError::Other("link bundle master key is not base64".into()))?;
    let master: Result<[u8; 32], _> = decoded.as_slice().try_into();
    decoded.zeroize();
    let master =
        master.map_err(|_| AppError::Other("link bundle master key is not 32 bytes".into()))?;

    if bundle.device_index == 0 || bundle.device_index as usize >= MAX_LINKED_DEVICES {
        return Err(AppError::Other(format!(
            "link bundle device index {} is out of range (1..{MAX_LINKED_DEVICES})",
            bundle.device_index
        )));
    }
    match device_index_of(&master, responder_node_id) {
        None => {
            return Err(
                "the linking device is not a device of the identity in the link bundle".into(),
            );
        }
        Some(index) if index == bundle.device_index => {
            return Err("link bundle assigns the index of the linking device".into());
        }
        Some(_) => {}
    }

    bundle.devices = own_devices(&master, &bundle.devices);
    let assigned_node_id = device_node_id(&master, bundle.device_index);
    if bundle.devices.iter().any(|d| d.node_id == assigned_node_id) {
        return Err("link bundle assigns a device index that is in use".into());
    }

    Ok(IdentityKeys {
        master_secret_key: master,
        device_index: bundle.device_index,
    })
}

/// Encrypt `plaintext` as a sequence of Noise transport messages, each at most
/// `NOISE_MAX_MESSAGE_LEN` bytes.
fn encrypt_chunks(
    transport: &mut snow::TransportState,
    plaintext: &[u8],
) -> Result<Vec<Vec<u8>>, snow::Error> {
    plaintext
        .chunks(NOISE_MAX_MESSAGE_LEN - NOISE_TAG_LEN)
        .map(|chunk| noise_transport_encrypt(transport, chunk))
        .collect()
}

/// Await `step` for at most `LINK_STEP_TIMEOUT`. `what` names the step in the error.
async fn link_step<T>(what: &str, step: impl Future<Output = T>) -> Result<T, AppError> {
    tokio::time::timeout(LINK_STEP_TIMEOUT, step)
        .await
        .map_err(|_| AppError::Other(format!("device link: {what} timed out")))
}

/// Write one chunk: `[u32 big-endian length][data]`. An empty `data` ends a chunk sequence.
async fn write_chunk(send: &mut SendStream, data: &[u8]) -> Result<(), AppError> {
    let len = u32::try_from(data.len())
        .map_err(|_| AppError::Other("device link: chunk too large".into()))?;
    link_step("write", async {
        send.write_all(&len.to_be_bytes()).await?;
        send.write_all(data).await
    })
    .await??;
    Ok(())
}

/// Read one chunk of at most `max_len` bytes. Returns `None` for the zero-length chunk
/// that ends a sequence.
async fn read_chunk(recv: &mut RecvStream, max_len: usize) -> Result<Option<Vec<u8>>, AppError> {
    link_step("read", async {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 {
            return Ok(None);
        }
        if len > max_len {
            return Err(AppError::Other(format!(
                "device link: chunk of {len} bytes exceeds the limit of {max_len}"
            )));
        }
        let mut buf = vec![0u8; len];
        recv.read_exact(&mut buf).await?;
        Ok(Some(buf))
    })
    .await?
}

/// Existing-device side, step 1: write the handshake response and `bundle_json`,
/// encrypted as Noise transport messages, as chunks, then finish the stream.
/// Returns the number of bundle chunks.
async fn send_link_bundle(
    send: &mut SendStream,
    transport: &mut snow::TransportState,
    noise_response: Vec<u8>,
    bundle_json: &[u8],
) -> Result<usize, AppError> {
    let chunks = encrypt_chunks(transport, bundle_json)?;
    let response = serde_json::to_vec(&PeerResponse::LinkBundle { noise_response })?;
    write_chunk(send, &response).await?;
    for chunk in &chunks {
        write_chunk(send, chunk).await?;
    }
    write_chunk(send, &[]).await?;
    send.finish()?;
    Ok(chunks.len())
}

/// Existing-device side, step 2: accept the confirmation stream and verify the key
/// confirmation. Only a peer that derived the transport keys, and therefore holds the
/// PSK, can produce a message that decrypts. Returns the stream for the acknowledgement.
async fn receive_link_confirmation(
    conn: &Connection,
    transport: &mut snow::TransportState,
) -> Result<SendStream, AppError> {
    let (ack_send, mut confirm_recv) =
        link_step("accept confirmation stream", conn.accept_bi()).await??;
    let confirmation = link_step(
        "read confirmation",
        confirm_recv.read_to_end(MAX_CONFIRM_LEN),
    )
    .await??;
    if noise_transport_decrypt(transport, &confirmation)? != LINK_CONFIRM {
        return Err("link confirmation has unexpected content".into());
    }
    Ok(ack_send)
}

/// Existing-device side, step 3: send the acknowledgement, then wait until the new
/// device closes the connection with `LINK_CLOSE_DONE`, which it does after it verified
/// the acknowledgement. Any other end of the connection is an error; the caller then
/// does not store the new device. If the new device completed anyway, its entry arrives
/// through device sync.
async fn acknowledge_link(
    mut ack_send: SendStream,
    transport: &mut snow::TransportState,
    conn: &Connection,
) -> Result<(), AppError> {
    let ack = noise_transport_encrypt(transport, LINK_ACK)?;
    link_step("write acknowledgement", ack_send.write_all(&ack)).await??;
    ack_send.finish()?;

    let closed = link_step("wait for completion", conn.closed()).await?;
    match &closed {
        ConnectionError::ApplicationClosed(close)
            if close.error_code == VarInt::from_u32(LINK_CLOSE_DONE) =>
        {
            Ok(())
        }
        _ => Err(AppError::Other(format!(
            "new device did not report completion: {closed}"
        ))),
    }
}

impl PeerHandler {
    /// Begin a device-link session. Returns the QR payload the existing device displays.
    /// `relay_url` should come from `endpoint.addr().relay_urls().next()`.
    /// A session that is already pending is replaced.
    pub async fn start_link_session(
        &self,
        relay_url: Option<String>,
    ) -> Result<LinkQrPayload, AppError> {
        let mut psk = [0u8; 32];
        getrandom::fill(&mut psk)?;

        let transport_node_id = self.identity.read().await.transport_node_id.clone();
        let expires_at = now_millis() + LINK_SESSION_TTL_MS;

        *self.pending_link.lock().await = Some(PendingLink {
            psk,
            expires_at,
            attempts: 0,
        });

        let payload = LinkQrPayload {
            node_id: transport_node_id,
            secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(psk),
            relay_url,
            expires_at,
        };
        psk.zeroize();

        log::info!(
            "[link] started device link session (expires in {}s)",
            LINK_SESSION_TTL_MS / 1000
        );
        Ok(payload)
    }

    /// Cancel an active device-link session.
    pub async fn cancel_link_session(&self) {
        let mut lock = self.pending_link.lock().await;
        *lock = None;
        log::info!("[link] cancelled device link session");
    }

    /// Decode and check `payload` without connecting. See `parse_link_payload`.
    pub fn check_link_payload(payload: &LinkQrPayload) -> Result<(), AppError> {
        parse_link_payload(payload, now_millis()).map(|_| ())
    }

    /// Existing-device side of device linking. Runs `serve_link_request`; on failure
    /// closes the connection with `LINK_CLOSE_FAILED`.
    pub(super) async fn handle_link_request(
        &self,
        send: SendStream,
        noise_init: Vec<u8>,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        match self.serve_link_request(send, &noise_init, conn).await {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!(
                    "[link] link request from {} failed: {e}",
                    short_id(&conn.remote_id().to_string())
                );
                conn.close(VarInt::from_u32(LINK_CLOSE_FAILED), b"link failed");
                Err(AcceptError::from_err(std::io::Error::other(e.to_string())))
            }
        }
    }

    /// Serve one link request: handshake, send the bundle, verify the key confirmation,
    /// remove the session, acknowledge, and store and announce the new device after the
    /// new device closed the connection with `LINK_CLOSE_DONE`.
    async fn serve_link_request(
        &self,
        mut send: SendStream,
        noise_init: &[u8],
        conn: &Connection,
    ) -> Result<(), AppError> {
        let psk = {
            let mut slot = self.pending_link.lock().await;
            begin_link_attempt(&mut slot, now_millis())?
        };

        let (master_secret_key_bytes, master_pubkey, device_index) = {
            let id = self.identity.read().await;
            (
                id.master_secret_key_bytes,
                id.master_pubkey.clone(),
                id.device_index,
            )
        };

        // This succeeds for any initiator: the first IKpsk2 message does not depend on
        // the PSK. The response and the transport keys do.
        let mut x25519_private = ed25519_secret_to_x25519(&derive_transport_key(
            &master_secret_key_bytes,
            device_index,
        ));
        let handshake = noise_psk_respond(&x25519_private, &psk, noise_init);
        x25519_private.zeroize();
        let (mut transport, noise_response) = handshake?;

        let devices = own_devices(
            &master_secret_key_bytes,
            &self.storage.get_linked_devices().await?,
        );
        let mut used_node_ids: HashSet<String> =
            devices.iter().map(|d| d.node_id.clone()).collect();
        used_node_ids.insert(device_node_id(&master_secret_key_bytes, device_index));
        let new_device_index = lowest_unused_device_index(&master_secret_key_bytes, &used_node_ids)
            .ok_or_else(|| {
                AppError::Other(format!("device limit of {MAX_LINKED_DEVICES} reached"))
            })?;
        let new_node_id = device_node_id(&master_secret_key_bytes, new_device_index);

        let lww = self.storage.load_lww_state(&master_pubkey).await?;
        let mut bundle = LinkBundleData {
            master_secret_key: base64::engine::general_purpose::STANDARD
                .encode(master_secret_key_bytes),
            device_index: new_device_index,
            devices,
            profile: self.storage.get_profile(&master_pubkey).await?,
            follows: lww.follows,
            moderation: lww.moderation,
            bookmarks: lww.bookmarks,
            peer_announcements: self
                .storage
                .get_followed_peer_announcements(&master_pubkey)
                .await?,
        };
        let bundle_json = serde_json::to_vec(&bundle);
        bundle.master_secret_key.zeroize();
        let mut bundle_json = bundle_json?;
        let sent = send_link_bundle(&mut send, &mut transport, noise_response, &bundle_json).await;
        let bundle_len = bundle_json.len();
        bundle_json.zeroize();
        let chunk_count = sent?;
        log::info!("[link] sent link bundle ({bundle_len} bytes in {chunk_count} chunks)");

        let ack_send = receive_link_confirmation(conn, &mut transport).await?;
        {
            let mut slot = self.pending_link.lock().await;
            if !consume_link_session(&mut slot, &psk) {
                return Err("link session is no longer pending".into());
            }
        }
        acknowledge_link(ack_send, &mut transport, conn).await?;

        self.storage
            .upsert_linked_device(
                &new_node_id,
                &crate::announcement::default_device_name(new_device_index),
                false,
                false,
                now_millis(),
            )
            .await?;
        log::info!(
            "[link] registered new device {} (index={new_device_index})",
            short_id(&new_node_id)
        );
        // The devices page closes the QR code and reloads the device list on this event.
        let _ = self.app_handle.emit("device-link-progress", "bundle_sent");

        match crate::announcement::build_own_announcement(&self.storage, &self.identity).await {
            Ok(announcement) => {
                if let Err(e) = self.gossip.broadcast_linked_devices(&announcement).await {
                    log::error!("[link] failed to broadcast device announcement: {e}");
                } else {
                    log::info!("[link] broadcast updated device announcement");
                }
            }
            Err(e) => log::error!("[link] failed to build device announcement: {e}"),
        }

        Ok(())
    }

    /// New-device side of device linking.
    ///
    /// Connects to the device named in `payload`, runs the Noise IKpsk2 handshake,
    /// receives and validates the link bundle, sends the key confirmation and verifies
    /// the acknowledgement. Returns the keys of the identity to install and the bundle
    /// (with `master_secret_key` cleared). Writes nothing to disk.
    pub async fn request_link(
        &self,
        endpoint: &Endpoint,
        payload: &LinkQrPayload,
    ) -> Result<(IdentityKeys, LinkBundleData), AppError> {
        let target = parse_link_payload(payload, now_millis())?;
        let responder_node_id = target.node_id.to_string();

        let mut my_x25519_private = {
            let id = self.identity.read().await;
            if id.device_index_of(&responder_node_id).is_some() {
                return Err("this device already belongs to the identity of that device".into());
            }
            ed25519_secret_to_x25519(&derive_transport_key(
                &id.master_secret_key_bytes,
                id.device_index,
            ))
        };
        let peer_x25519_public = ed25519_public_to_x25519(target.node_id.as_bytes())
            .ok_or("link code NodeId is not a valid public key")?;
        let initiated = noise_psk_initiate(&my_x25519_private, &peer_x25519_public, &target.psk);
        my_x25519_private.zeroize();
        let (handshake, noise_init) = initiated?;

        let conn = link_step("connect", endpoint.connect(target.addr, PEER_ALPN)).await??;
        let result = exchange_link(&conn, handshake, noise_init, &responder_node_id).await;
        match &result {
            Ok(_) => conn.close(VarInt::from_u32(LINK_CLOSE_DONE), b"done"),
            Err(_) => conn.close(VarInt::from_u32(LINK_CLOSE_FAILED), b"link failed"),
        }
        result
    }
}

/// Run the new-device side of the link exchange on an established connection.
async fn exchange_link(
    conn: &Connection,
    handshake: snow::HandshakeState,
    noise_init: Vec<u8>,
    responder_node_id: &str,
) -> Result<(IdentityKeys, LinkBundleData), AppError> {
    let (mut send, mut recv) = link_step("open stream", conn.open_bi()).await??;
    let request = serde_json::to_vec(&PeerRequest::LinkRequest { noise_init })?;
    link_step("write request", send.write_all(&request)).await??;
    send.finish()?;

    let response = read_chunk(&mut recv, MAX_LINK_RESPONSE_LEN)
        .await?
        .ok_or("empty link response")?;
    let PeerResponse::LinkBundle { noise_response } = serde_json::from_slice(&response)? else {
        return Err("unexpected response type".into());
    };
    // Fails if the two devices do not hold the same PSK.
    let mut transport = noise_psk_complete_initiator(handshake, &noise_response)?;

    let mut bundle_json: Vec<u8> = Vec::new();
    while let Some(chunk) = read_chunk(&mut recv, NOISE_MAX_MESSAGE_LEN).await? {
        let mut plaintext = match noise_transport_decrypt(&mut transport, &chunk) {
            Ok(plaintext) => plaintext,
            Err(e) => {
                bundle_json.zeroize();
                return Err(e.into());
            }
        };
        let fits = bundle_json.len() + plaintext.len() <= MAX_BUNDLE_LEN;
        if fits {
            bundle_json.extend_from_slice(&plaintext);
        }
        plaintext.zeroize();
        if !fits {
            bundle_json.zeroize();
            return Err(AppError::Other(format!(
                "link bundle exceeds {MAX_BUNDLE_LEN} bytes"
            )));
        }
    }
    let parsed: Result<LinkBundleData, _> = serde_json::from_slice(&bundle_json);
    bundle_json.zeroize();
    let mut bundle = parsed?;
    let keys = validate_link_bundle(&mut bundle, responder_node_id)?;
    log::info!(
        "[link] received link bundle (device index {})",
        keys.device_index
    );

    // Key confirmation: proves to the existing device that this device holds the PSK
    // and decrypted the bundle.
    let (mut confirm_send, mut ack_recv) =
        link_step("open confirmation stream", conn.open_bi()).await??;
    let confirmation = noise_transport_encrypt(&mut transport, LINK_CONFIRM)?;
    link_step("write confirmation", confirm_send.write_all(&confirmation)).await??;
    confirm_send.finish()?;

    let ack = link_step(
        "read acknowledgement",
        ack_recv.read_to_end(MAX_CONFIRM_LEN),
    )
    .await??;
    if noise_transport_decrypt(&mut transport, &ack)? != LINK_ACK {
        return Err("link acknowledgement has unexpected content".into());
    }

    Ok((keys, bundle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::loopback_endpoint;
    use proscenium_types::DeviceEntry;

    fn x25519_of_device(master: &[u8; 32], index: u32) -> ([u8; 32], [u8; 32]) {
        let private = ed25519_secret_to_x25519(&derive_transport_key(master, index));
        let node_id: EndpointId = device_node_id(master, index).parse().unwrap();
        let public = ed25519_public_to_x25519(node_id.as_bytes()).unwrap();
        (private, public)
    }

    fn device(master: &[u8; 32], index: u32) -> DeviceEntry {
        DeviceEntry {
            node_id: device_node_id(master, index),
            device_name: format!("Device {index}"),
            is_primary: false,
            added_at: 1,
        }
    }

    fn bundle(master: &[u8; 32], device_index: u32, devices: Vec<DeviceEntry>) -> LinkBundleData {
        LinkBundleData {
            master_secret_key: base64::engine::general_purpose::STANDARD.encode(master),
            device_index,
            devices,
            profile: None,
            follows: vec![],
            moderation: vec![],
            bookmarks: vec![],
            peer_announcements: vec![],
        }
    }

    /// Existing-device side of one link exchange on `endpoint`: the request read of
    /// `PeerHandler::accept`, then the three steps of `serve_link_request`, without
    /// session and storage.
    async fn serve_one_link(
        endpoint: Endpoint,
        psk: [u8; 32],
        x25519_private: [u8; 32],
        bundle_json: Vec<u8>,
    ) -> Result<(), AppError> {
        let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
        let conn = incoming.await.map_err(|e| AppError::Other(e.to_string()))?;
        let (mut send, mut recv) = conn.accept_bi().await?;
        let request = recv.read_to_end(10_000).await?;
        let PeerRequest::LinkRequest { noise_init } = serde_json::from_slice(&request)? else {
            return Err("unexpected request".into());
        };
        let (mut transport, noise_response) =
            noise_psk_respond(&x25519_private, &psk, &noise_init)?;
        send_link_bundle(&mut send, &mut transport, noise_response, &bundle_json).await?;
        let ack_send = receive_link_confirmation(&conn, &mut transport).await?;
        acknowledge_link(ack_send, &mut transport, &conn).await
    }

    /// Two loopback endpoints: the existing device (master `EXISTING_MASTER`, serving one
    /// link exchange) and the new device, connected to it.
    struct LinkPair {
        existing: Endpoint,
        new: Endpoint,
        conn: Connection,
        responder_node_id: String,
        server: tokio::task::JoinHandle<Result<(), AppError>>,
    }

    const EXISTING_MASTER: [u8; 32] = [7u8; 32];
    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// Start the existing device with `responder_psk` and connect the new device.
    /// Also returns the new device's handshake, started with `initiator_psk`.
    async fn link_pair(
        responder_psk: [u8; 32],
        initiator_psk: [u8; 32],
        bundle: &LinkBundleData,
    ) -> (LinkPair, snow::HandshakeState, Vec<u8>) {
        let new_master = [8u8; 32];
        let existing = loopback_endpoint(
            iroh::SecretKey::from_bytes(&derive_transport_key(&EXISTING_MASTER, 0)),
            PEER_ALPN,
        )
        .await;
        let new = loopback_endpoint(
            iroh::SecretKey::from_bytes(&derive_transport_key(&new_master, 0)),
            PEER_ALPN,
        )
        .await;
        let (existing_private, existing_public) = x25519_of_device(&EXISTING_MASTER, 0);
        let (new_private, _) = x25519_of_device(&new_master, 0);

        let server = tokio::spawn(serve_one_link(
            existing.clone(),
            responder_psk,
            existing_private,
            serde_json::to_vec(bundle).unwrap(),
        ));
        let (handshake, noise_init) =
            noise_psk_initiate(&new_private, &existing_public, &initiator_psk).unwrap();
        let conn = tokio::time::timeout(TEST_TIMEOUT, new.connect(existing.addr(), PEER_ALPN))
            .await
            .expect("connect timed out")
            .expect("connect");
        let pair = LinkPair {
            responder_node_id: existing.id().to_string(),
            existing,
            new,
            conn,
            server,
        };
        (pair, handshake, noise_init)
    }

    impl LinkPair {
        /// Run the new-device side of the exchange.
        async fn exchange(
            &self,
            handshake: snow::HandshakeState,
            noise_init: Vec<u8>,
        ) -> Result<(IdentityKeys, LinkBundleData), AppError> {
            tokio::time::timeout(
                TEST_TIMEOUT,
                exchange_link(&self.conn, handshake, noise_init, &self.responder_node_id),
            )
            .await
            .expect("exchange timed out")
        }

        /// Close the connection with `code`, return the result of the existing-device
        /// task, and close both endpoints.
        async fn close(self, code: u32) -> Result<(), AppError> {
            self.conn.close(VarInt::from_u32(code), b"");
            let served = tokio::time::timeout(TEST_TIMEOUT, self.server)
                .await
                .expect("server task timed out")
                .expect("server task panicked");
            self.new.close().await;
            self.existing.close().await;
            served
        }
    }

    #[tokio::test]
    async fn link_exchange_over_loopback_transfers_the_bundle() {
        let psk = [9u8; 32];
        // More follow rows than fit into one Noise message.
        let mut sent = bundle(&EXISTING_MASTER, 1, vec![device(&EXISTING_MASTER, 0)]);
        sent.follows = (0..2_000u32)
            .map(|i| proscenium_types::FollowEntry {
                pubkey: format!("{i:064}"),
                followed_at: 1,
                state: "active".to_string(),
                last_changed_at: 1,
            })
            .collect();
        assert!(serde_json::to_vec(&sent).unwrap().len() > NOISE_MAX_MESSAGE_LEN);

        let (pair, handshake, noise_init) = link_pair(psk, psk, &sent).await;
        let Ok((keys, received)) = pair.exchange(handshake, noise_init).await else {
            panic!("exchange failed");
        };
        assert_eq!(keys.master_secret_key, EXISTING_MASTER);
        assert_eq!(keys.device_index, 1);
        assert_eq!(received.follows.len(), 2_000);
        assert_eq!(received.devices.len(), 1);
        assert!(received.master_secret_key.is_empty());

        // The existing device saw the confirmation and the completion close.
        pair.close(LINK_CLOSE_DONE)
            .await
            .expect("existing device side");
    }

    #[tokio::test]
    async fn link_exchange_with_wrong_psk_fails_on_both_devices() {
        let sent = bundle(&EXISTING_MASTER, 1, vec![device(&EXISTING_MASTER, 0)]);
        let (pair, handshake, noise_init) = link_pair([1u8; 32], [2u8; 32], &sent).await;
        assert!(pair.exchange(handshake, noise_init).await.is_err());
        // No confirmation arrived, so the existing device reports an error.
        assert!(pair.close(LINK_CLOSE_FAILED).await.is_err());
    }

    #[tokio::test]
    async fn existing_device_requires_the_completion_close_code() {
        let psk = [9u8; 32];
        let sent = bundle(&EXISTING_MASTER, 1, vec![device(&EXISTING_MASTER, 0)]);
        let (pair, handshake, noise_init) = link_pair(psk, psk, &sent).await;
        assert!(pair.exchange(handshake, noise_init).await.is_ok());
        // Code 0 is what a dropped connection sends.
        assert!(pair.close(0).await.is_err());
    }

    #[test]
    fn session_allows_five_attempts_and_survives_failed_ones() {
        let psk = [4u8; 32];
        let mut slot = Some(PendingLink {
            psk,
            expires_at: 1_000,
            attempts: 0,
        });
        for _ in 0..MAX_LINK_ATTEMPTS {
            assert_eq!(begin_link_attempt(&mut slot, 500).unwrap(), psk);
            assert!(slot.is_some());
        }
        assert!(begin_link_attempt(&mut slot, 500).is_err());
        assert!(slot.is_none());
        assert!(begin_link_attempt(&mut slot, 500).is_err());
    }

    #[test]
    fn expired_session_is_removed() {
        let mut slot = Some(PendingLink {
            psk: [4u8; 32],
            expires_at: 1_000,
            attempts: 0,
        });
        assert!(begin_link_attempt(&mut slot, 1_001).is_err());
        assert!(slot.is_none());
    }

    #[test]
    fn session_is_consumed_once_and_only_by_its_psk() {
        let mut slot = Some(PendingLink {
            psk: [4u8; 32],
            expires_at: 1_000,
            attempts: 1,
        });
        assert!(!consume_link_session(&mut slot, &[5u8; 32]));
        assert!(slot.is_some());
        assert!(consume_link_session(&mut slot, &[4u8; 32]));
        assert!(!consume_link_session(&mut slot, &[4u8; 32]));
    }

    #[test]
    fn device_index_is_lowest_unused() {
        let master = [1u8; 32];
        let used = |indexes: &[u32]| -> HashSet<String> {
            indexes
                .iter()
                .map(|i| device_node_id(&master, *i))
                .collect()
        };
        assert_eq!(lowest_unused_device_index(&master, &used(&[0])), Some(1));
        assert_eq!(
            lowest_unused_device_index(&master, &used(&[0, 1, 2])),
            Some(3)
        );
        // Index 1 is free again although three devices are stored.
        assert_eq!(
            lowest_unused_device_index(&master, &used(&[0, 2, 3])),
            Some(1)
        );
        let all: Vec<u32> = (0..MAX_LINKED_DEVICES as u32).collect();
        assert_eq!(lowest_unused_device_index(&master, &used(&all)), None);
    }

    #[test]
    fn link_payload_is_validated() {
        let master = [2u8; 32];
        let good = LinkQrPayload {
            node_id: device_node_id(&master, 0),
            secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9u8; 32]),
            relay_url: Some("not a url".to_string()),
            expires_at: 2_000,
        };
        let target = parse_link_payload(&good, 1_000).unwrap();
        assert_eq!(target.psk, [9u8; 32]);
        assert_eq!(target.node_id.to_string(), good.node_id);
        assert_eq!(target.addr.relay_urls().count(), 0);

        assert!(parse_link_payload(&good, 2_000).is_err());
        let bad_node = LinkQrPayload {
            node_id: "aaaaaaa\u{20ac}".to_string(),
            ..good.clone()
        };
        assert!(parse_link_payload(&bad_node, 1_000).is_err());
        let short_psk = LinkQrPayload {
            secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9u8; 31]),
            ..good.clone()
        };
        assert!(parse_link_payload(&short_psk, 1_000).is_err());
        let with_relay = LinkQrPayload {
            relay_url: Some("https://relay.example.org/".to_string()),
            ..good
        };
        let target = parse_link_payload(&with_relay, 1_000).unwrap();
        assert_eq!(target.addr.relay_urls().count(), 1);
    }

    #[test]
    fn link_bundle_is_validated() {
        let master = [3u8; 32];
        let responder = device_node_id(&master, 0);
        let foreign = device(&[4u8; 32], 0);

        let mut ok = bundle(&master, 2, vec![device(&master, 0), foreign]);
        let keys = validate_link_bundle(&mut ok, &responder).unwrap();
        assert_eq!(keys.master_secret_key, master);
        assert_eq!(keys.device_index, 2);
        assert!(ok.master_secret_key.is_empty());
        // The foreign entry is dropped and index 0 is marked as primary.
        assert_eq!(ok.devices.len(), 1);
        assert!(ok.devices[0].is_primary);

        // Index 0 and indexes outside the range are rejected.
        for index in [0, MAX_LINKED_DEVICES as u32, u32::MAX] {
            assert!(validate_link_bundle(&mut bundle(&master, index, vec![]), &responder).is_err());
        }
        // A master key of the wrong length.
        let mut short = bundle(&master, 1, vec![]);
        short.master_secret_key = base64::engine::general_purpose::STANDARD.encode([3u8; 31]);
        assert!(validate_link_bundle(&mut short, &responder).is_err());
        // The responder is not a device of the bundle's identity.
        let other = device_node_id(&[4u8; 32], 0);
        assert!(validate_link_bundle(&mut bundle(&master, 1, vec![]), &other).is_err());
        // The assigned index is the responder's own, or already listed.
        let responder_1 = device_node_id(&master, 1);
        assert!(validate_link_bundle(&mut bundle(&master, 1, vec![]), &responder_1).is_err());
        let mut in_use = bundle(&master, 2, vec![device(&master, 2)]);
        assert!(validate_link_bundle(&mut in_use, &responder).is_err());
    }

    #[test]
    fn bundle_larger_than_one_noise_message_round_trips() {
        let existing = [7u8; 32];
        let new = [8u8; 32];
        let psk = [9u8; 32];
        let (existing_private, existing_public) = x25519_of_device(&existing, 0);
        let (new_private, _) = x25519_of_device(&new, 0);

        let (handshake, init) = noise_psk_initiate(&new_private, &existing_public, &psk).unwrap();
        let (mut responder, response) = noise_psk_respond(&existing_private, &psk, &init).unwrap();
        let mut initiator = noise_psk_complete_initiator(handshake, &response).unwrap();

        let plaintext: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let chunks = encrypt_chunks(&mut responder, &plaintext).unwrap();
        assert_eq!(chunks.len(), 4);
        assert!(chunks.iter().all(|c| c.len() <= NOISE_MAX_MESSAGE_LEN));
        let mut decrypted = Vec::new();
        for chunk in &chunks {
            decrypted.extend(noise_transport_decrypt(&mut initiator, chunk).unwrap());
        }
        assert_eq!(decrypted, plaintext);

        // Confirmation and acknowledgement in the two directions.
        let confirmation = noise_transport_encrypt(&mut initiator, LINK_CONFIRM).unwrap();
        assert_eq!(
            noise_transport_decrypt(&mut responder, &confirmation).unwrap(),
            LINK_CONFIRM
        );
        let ack = noise_transport_encrypt(&mut responder, LINK_ACK).unwrap();
        assert_eq!(
            noise_transport_decrypt(&mut initiator, &ack).unwrap(),
            LINK_ACK
        );
    }

    #[test]
    fn initiator_without_the_psk_cannot_complete_or_confirm() {
        let existing = [7u8; 32];
        let new = [8u8; 32];
        let (existing_private, existing_public) = x25519_of_device(&existing, 0);
        let (new_private, _) = x25519_of_device(&new, 0);

        // The responder accepts the first message of an initiator with another PSK.
        let (handshake, init) =
            noise_psk_initiate(&new_private, &existing_public, &[1u8; 32]).unwrap();
        let (mut responder, response) =
            noise_psk_respond(&existing_private, &[2u8; 32], &init).unwrap();
        // The initiator cannot derive the transport keys.
        assert!(noise_psk_complete_initiator(handshake, &response).is_err());
        // Bytes that were not encrypted with the transport keys do not pass as confirmation.
        assert!(noise_transport_decrypt(&mut responder, &[0u8; 41]).is_err());
        assert!(noise_transport_decrypt(&mut responder, LINK_CONFIRM).is_err());
    }
}
