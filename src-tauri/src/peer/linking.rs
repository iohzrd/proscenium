//! Device linking over the peer ALPN.
//!
//! The existing device shows a QR code with its NodeId and a one-time secret. The new
//! device dials that NodeId, so QUIC authenticates the existing device; the secret
//! authenticates the new device. The new device generates a new transport key for the
//! identity, so a key that a device list of a previous identity names is not reused, and
//! sends its NodeId; the existing device stores that NodeId as the new device.
//!
//! Both devices show `link_code` of the existing NodeId and the new NodeId, and the
//! existing device sends the master key only after the user confirmed in a native dialog
//! that the codes match. The new NodeId is generated after the QR code was read and sent
//! only inside the QUIC connection, so a device that also read the QR code cannot choose a
//! NodeId with the same code.
//!
//! One bi-stream, opened by the new device:
//! - new device: JSON `PeerRequest::LinkRequest { secret, node_id, device_name }`, then
//!   finish.
//! - existing device: compares the secret in constant time and removes the session, asks
//!   for confirmation, then writes the `LinkBundleData` JSON and finishes, or closes the
//!   connection with `LINK_CLOSE_DECLINED`.
//!
//! The new device validates the bundle and closes the connection with `LINK_CLOSE_DONE`.
//! The existing device stores the new device only after it received that close code.

use crate::announcement::{build_own_announcement, local_device_name, verify_lists_device};
use crate::error::AppError;
use crate::setup::IdentityKeys;
use base64::Engine;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey,
    endpoint::{Connection, ConnectionError, SendStream, VarInt},
    protocol::AcceptError,
};
use proscenium_types::{
    LinkBundleData, LinkQrPayload, MAX_LINKED_DEVICES, PEER_ALPN, PeerRequest, now_millis,
    parse_pubkey, short_id,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tauri::Emitter;
use zeroize::Zeroize;

use super::PeerHandler;

/// Device-link session on the existing device, created by `start_link_session`.
///
/// It is removed when a request presents its secret, when a request arrives after it
/// expired, or when it is cancelled or replaced. A remote NodeId that made
/// `MAX_LINK_ATTEMPTS` attempts is refused; other remotes can still present the secret.
pub(super) struct PendingLink {
    secret: [u8; 32],
    expires_at: u64,
    /// Attempts per remote NodeId, for at most `MAX_LINK_REMOTES` remotes.
    attempts: HashMap<EndpointId, u32>,
}

impl PendingLink {
    fn new(secret: [u8; 32], expires_at: u64) -> Self {
        Self {
            secret,
            expires_at,
            attempts: HashMap::new(),
        }
    }
}

pub(super) type PendingLinkState = Arc<tokio::sync::Mutex<Option<PendingLink>>>;

pub(super) const LINK_SESSION_TTL_MS: u64 = 60_000; // 60 seconds
/// Link requests served per session and remote NodeId. A request with a wrong secret
/// counts as one. The secret has 256 random bits, so the limit bounds work, not guessing.
const MAX_LINK_ATTEMPTS: u32 = 5;
/// Remote NodeIds counted per session; requests of further remotes are refused.
const MAX_LINK_REMOTES: usize = 32;
/// Time limit for each network step of the link exchange except the bundle transfer.
const LINK_STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// Time limit for writing or reading the link bundle.
const LINK_BUNDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Time the existing device waits for the user to confirm the link code.
const LINK_CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

/// Domain tag of the `link_code` hash.
const LINK_CODE_TAG: &[u8] = b"proscenium/link-code/1";

/// Largest accepted `LinkBundleData` JSON.
const MAX_BUNDLE_LEN: usize = 16 * 1024 * 1024;

/// QUIC close code sent by the new device after it validated the bundle.
/// Not 0: dropping a connection without `close` sends code 0.
const LINK_CLOSE_DONE: u32 = 1;
/// QUIC close code sent by either device when the exchange failed.
const LINK_CLOSE_FAILED: u32 = 2;
/// QUIC close code sent by the existing device when the user declined the link code.
const LINK_CLOSE_DECLINED: u32 = 3;

/// Six-digit code that both devices show before the master key is sent: SHA-256 over
/// `LINK_CODE_TAG`, the existing device's NodeId and the NodeId in the link request; the
/// first 4 bytes as a big-endian integer modulo 10^6, as two groups of 3 digits.
fn link_code(existing: &EndpointId, requested: &EndpointId) -> String {
    let digest = Sha256::new()
        .chain_update(LINK_CODE_TAG)
        .chain_update(existing.as_bytes())
        .chain_update(requested.as_bytes())
        .finalize();
    let number = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03} {:03}", number / 1_000, number % 1_000)
}

/// End of a served link request on the existing device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkServed {
    /// The bundle was sent and the new device reported completion.
    Linked,
    /// The user declined the link code. Nothing was sent.
    Declined,
}

/// Count one link attempt of `remote` against the session in `slot` and return its
/// secret. Removes the session if it expired. Refuses a remote that made
/// `MAX_LINK_ATTEMPTS` attempts, and a new remote once `MAX_LINK_REMOTES` are counted.
fn begin_link_attempt(
    slot: &mut Option<PendingLink>,
    remote: EndpointId,
    now: u64,
) -> Result<[u8; 32], AppError> {
    let Some(session) = slot.as_mut() else {
        return Err("no pending link session".into());
    };
    if now > session.expires_at {
        *slot = None;
        return Err("link session expired".into());
    }
    if !session.attempts.contains_key(&remote) && session.attempts.len() >= MAX_LINK_REMOTES {
        return Err("too many devices tried this link session".into());
    }
    let attempts = session.attempts.entry(remote).or_insert(0);
    if *attempts >= MAX_LINK_ATTEMPTS {
        return Err("too many link attempts from this device".into());
    }
    *attempts += 1;
    Ok(session.secret)
}

/// Remove the session in `slot` if it holds `secret`. Returns `false` if the session was
/// cancelled, replaced, or removed by another request in the meantime.
fn consume_link_session(slot: &mut Option<PendingLink>, secret: &[u8; 32]) -> bool {
    let matches = slot
        .as_ref()
        .is_some_and(|session| session.secret == *secret);
    if matches {
        *slot = None;
    }
    matches
}

/// Decode a link secret (URL-safe base64 without padding, 32 bytes).
fn decode_secret(encoded: &str) -> Result<[u8; 32], AppError> {
    let mut decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| AppError::Other("link secret is not base64".into()))?;
    let secret: Result<[u8; 32], _> = decoded.as_slice().try_into();
    decoded.zeroize();
    secret.map_err(|_| AppError::Other("link secret is not 32 bytes".into()))
}

/// Decoded fields of a `LinkQrPayload`.
struct LinkTarget {
    node_id: EndpointId,
    secret: [u8; 32],
    addr: EndpointAddr,
}

/// Decode and check a `LinkQrPayload`: the NodeId parses, the secret is 32 bytes, and
/// `expires_at` is later than `now`. A relay URL that does not parse is ignored.
fn parse_link_payload(payload: &LinkQrPayload, now: u64) -> Result<LinkTarget, AppError> {
    let node_id: EndpointId = payload
        .node_id
        .parse()
        .map_err(|_| AppError::Other("link code holds an invalid NodeId".into()))?;
    let secret = decode_secret(&payload.secret)?;
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
    Ok(LinkTarget {
        node_id,
        secret,
        addr,
    })
}

/// Check a link bundle received from the device `responder_node_id` and return the keys
/// of the identity it defines, with the transport key `transport_secret_key` of this
/// device.
///
/// Fails unless the master key is 32 bytes and `bundle.announcement` is signed by it and
/// lists the responder. `bundle.master_secret_key` is cleared.
fn validate_link_bundle(
    bundle: &mut LinkBundleData,
    responder_node_id: &str,
    transport_secret_key: [u8; 32],
) -> Result<IdentityKeys, AppError> {
    let decoded =
        base64::engine::general_purpose::STANDARD.decode(bundle.master_secret_key.as_bytes());
    bundle.master_secret_key.zeroize();
    let mut decoded =
        decoded.map_err(|_| AppError::Other("link bundle master key is not base64".into()))?;
    let master: Result<[u8; 32], _> = decoded.as_slice().try_into();
    decoded.zeroize();
    let keys = IdentityKeys {
        master_secret_key: master
            .map_err(|_| AppError::Other("link bundle master key is not 32 bytes".into()))?,
        transport_secret_key,
    };
    let master_pubkey = SecretKey::from_bytes(&keys.master_secret_key)
        .public()
        .to_string();
    verify_lists_device(&bundle.announcement, &master_pubkey, responder_node_id)
        .map_err(|e| AppError::Other(format!("link bundle device list rejected: {e}")))?;
    Ok(keys)
}

/// Await `step` for at most `limit`. `what` names the step in the error.
async fn link_step<T>(
    what: &str,
    limit: Duration,
    step: impl Future<Output = T>,
) -> Result<T, AppError> {
    tokio::time::timeout(limit, step)
        .await
        .map_err(|_| AppError::Other(format!("device link: {what} timed out")))
}

/// Existing-device side: check that `presented`, sent by `remote`, equals the session
/// secret and remove the session. The comparison takes the same time for every presented
/// value.
async fn check_link_secret(
    pending: &PendingLinkState,
    remote: EndpointId,
    presented: &str,
) -> Result<(), AppError> {
    let secret = {
        let mut slot = pending.lock().await;
        begin_link_attempt(&mut slot, remote, now_millis())?
    };
    let mut presented = decode_secret(presented)?;
    let matches: bool = presented.ct_eq(&secret).into();
    presented.zeroize();
    if !matches {
        return Err("link secret does not match".into());
    }
    let mut slot = pending.lock().await;
    if !consume_link_session(&mut slot, &secret) {
        return Err("link session is no longer pending".into());
    }
    Ok(())
}

/// True if the other device closed `conn` with `LINK_CLOSE_DECLINED`.
fn was_declined(conn: &Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(close))
            if close.error_code == VarInt::from_u32(LINK_CLOSE_DECLINED)
    )
}

/// Existing-device side: write `bundle_json` and finish the stream.
async fn send_link_bundle(send: &mut SendStream, bundle_json: &[u8]) -> Result<(), AppError> {
    link_step(
        "write bundle",
        LINK_BUNDLE_TIMEOUT,
        send.write_all(bundle_json),
    )
    .await??;
    send.finish()?;
    Ok(())
}

/// Existing-device side: wait until the new device closes the connection with
/// `LINK_CLOSE_DONE`, which it does after it validated the bundle. Any other end of the
/// connection is an error; the caller then does not store the new device. If the new
/// device completed anyway, its entry arrives through device sync.
async fn wait_for_completion(conn: &Connection) -> Result<(), AppError> {
    let closed = link_step("wait for completion", LINK_BUNDLE_TIMEOUT, conn.closed()).await?;
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
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret)?;

        let transport_node_id = self.identity.read().await.transport_node_id.clone();
        let expires_at = now_millis() + LINK_SESSION_TTL_MS;

        *self.pending_link.lock().await = Some(PendingLink::new(secret, expires_at));

        let payload = LinkQrPayload {
            node_id: transport_node_id,
            secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret),
            relay_url,
            expires_at,
        };
        secret.zeroize();

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
        secret: String,
        node_id: &str,
        device_name: &str,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        let mut secret = secret;
        let served = self
            .serve_link_request(send, &secret, node_id, device_name, conn)
            .await;
        secret.zeroize();
        match served {
            Ok(LinkServed::Linked) => Ok(()),
            Ok(LinkServed::Declined) => {
                log::info!("[link] link code declined");
                conn.close(VarInt::from_u32(LINK_CLOSE_DECLINED), b"link declined");
                Err(AcceptError::from_err(std::io::Error::other(
                    "link code declined",
                )))
            }
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

    /// Serve one link request: check the secret and remove the session, then run
    /// `confirm_and_send`.
    ///
    /// Emits `device-link-progress`: `confirming` before the code dialog, then `declined`
    /// or `failed` if the link did not complete. A request refused before the dialog emits
    /// nothing, so it does not end a link whose dialog is shown.
    async fn serve_link_request(
        &self,
        send: SendStream,
        secret: &str,
        new_node_id: &str,
        device_name: &str,
        conn: &Connection,
    ) -> Result<LinkServed, AppError> {
        check_link_secret(&self.pending_link, conn.remote_id(), secret).await?;

        let new_node = parse_pubkey(new_node_id).map_err(AppError::Other)?;
        let new_node_id = new_node.to_string();
        let local_node = self.identity.read().await.transport_key.public();
        if new_node == local_node {
            return Err("link request names the local NodeId".into());
        }
        if self.storage.is_removed_device(&new_node_id).await? {
            return Err("link request names a removed NodeId".into());
        }
        let devices = self.storage.get_linked_devices().await?;
        if devices.len() >= MAX_LINKED_DEVICES && !devices.iter().any(|d| d.node_id == new_node_id)
        {
            return Err(AppError::Other(format!(
                "device limit of {MAX_LINKED_DEVICES} reached"
            )));
        }

        // The QR code is no longer accepted; the devices page closes it on this event.
        let _ = self.app_handle.emit("device-link-progress", "confirming");
        let served = self
            .confirm_and_send(
                send,
                conn,
                &link_code(&local_node, &new_node),
                &new_node_id,
                device_name,
            )
            .await;
        match &served {
            Ok(LinkServed::Linked) => {}
            Ok(LinkServed::Declined) => {
                let _ = self.app_handle.emit("device-link-progress", "declined");
            }
            Err(_) => {
                let _ = self.app_handle.emit("device-link-progress", "failed");
            }
        }
        served
    }

    /// Ask the user to compare `code` (`confirm_link`), send the bundle, and store
    /// `new_node_id` as the device `device_name` and announce it after the new device
    /// closed the connection with `LINK_CLOSE_DONE`.
    async fn confirm_and_send(
        &self,
        mut send: SendStream,
        conn: &Connection,
        code: &str,
        new_node_id: &str,
        device_name: &str,
    ) -> Result<LinkServed, AppError> {
        if !self.confirm_link(conn, code, device_name).await? {
            return Ok(LinkServed::Declined);
        }

        let master_pubkey = self.identity.read().await.master_pubkey.clone();
        let lww = self.storage.load_lww_state(&master_pubkey).await?;
        let announcement = build_own_announcement(&self.storage, &self.identity).await?;
        let profile = self.storage.get_profile(&master_pubkey).await?;
        let peer_announcements = self
            .storage
            .get_followed_peer_announcements(&master_pubkey)
            .await?;
        // The copy of the master key exists only while it is encoded.
        let mut master_secret_key_bytes = self.identity.read().await.master_secret_key_bytes;
        let master_secret_key =
            base64::engine::general_purpose::STANDARD.encode(master_secret_key_bytes);
        master_secret_key_bytes.zeroize();
        let mut bundle = LinkBundleData {
            master_secret_key,
            announcement,
            profile,
            follows: lww.follows,
            moderation: lww.moderation,
            bookmarks: lww.bookmarks,
            follow_requests: lww.follow_requests,
            peer_announcements,
        };
        let bundle_json = serde_json::to_vec(&bundle);
        bundle.master_secret_key.zeroize();
        let mut bundle_json = bundle_json?;
        let sent = send_link_bundle(&mut send, &bundle_json).await;
        let bundle_len = bundle_json.len();
        bundle_json.zeroize();
        sent?;
        log::info!("[link] sent link bundle ({bundle_len} bytes)");

        wait_for_completion(conn).await?;

        self.storage
            .upsert_linked_device(new_node_id, device_name, false, now_millis())
            .await?;
        log::info!("[link] registered new device {}", short_id(new_node_id));
        // The devices page closes the QR code and reloads the device list on this event.
        let _ = self.app_handle.emit("device-link-progress", "bundle_sent");

        match build_own_announcement(&self.storage, &self.identity).await {
            Ok(announcement) => {
                if let Err(e) = self.gossip.broadcast_linked_devices(&announcement).await {
                    log::error!("[link] failed to broadcast device announcement: {e}");
                } else {
                    log::info!("[link] broadcast updated device announcement");
                }
            }
            Err(e) => log::error!("[link] failed to build device announcement: {e}"),
        }

        Ok(LinkServed::Linked)
    }

    /// Ask in a native dialog whether the new device shows `code`. Returns `false` if the
    /// user declined, and an error if the connection closed or `LINK_CONFIRM_TIMEOUT`
    /// passed first; the dialog then stays open and its answer is not used.
    async fn confirm_link(
        &self,
        conn: &Connection,
        code: &str,
        device_name: &str,
    ) -> Result<bool, AppError> {
        let message = format!(
            "A device ({}) requests the master key of this identity.\n\n\
             Link it only if it shows the code {code}. If it shows another code, choose \
             Cancel: another device read the link code.",
            crate::confirm::dialog_device_name(device_name)
        );
        tokio::select! {
            confirmed = crate::confirm::confirm_native(
                &self.app_handle,
                "Link a device",
                &message,
                "Link device",
            ) => Ok(confirmed),
            closed = conn.closed() => Err(AppError::Other(format!(
                "new device closed the connection before the link was confirmed: {closed}"
            ))),
            () = tokio::time::sleep(LINK_CONFIRM_TIMEOUT) => {
                Err("the link code was not confirmed in time".into())
            }
        }
    }

    /// New-device side of device linking.
    ///
    /// Generates the transport key of the new identity, connects to the device named in
    /// `payload`, sends the secret and the NodeId of that key, emits `device-link-code`
    /// with the `link_code` to compare, and receives and validates the link bundle.
    /// Returns the keys of the identity to install and the bundle (with
    /// `master_secret_key` cleared). Writes nothing to disk.
    pub async fn request_link(
        &self,
        endpoint: &Endpoint,
        payload: &LinkQrPayload,
    ) -> Result<(IdentityKeys, LinkBundleData), AppError> {
        let mut target = parse_link_payload(payload, now_millis())?;
        let responder_node_id = target.node_id.to_string();
        if self.storage.is_other_device(&responder_node_id).await? {
            target.secret.zeroize();
            return Err("this device already belongs to the identity of that device".into());
        }
        let mut transport_secret_key = crate::setup::random_key()?;

        // QUIC authenticates the NodeId of the link code.
        let connected = link_step(
            "connect",
            LINK_STEP_TIMEOUT,
            endpoint.connect(target.addr, PEER_ALPN),
        )
        .await;
        let result = match connected {
            Ok(Ok(conn)) => {
                let app_handle = self.app_handle.clone();
                let result = exchange_link(
                    &conn,
                    &target.secret,
                    target.node_id,
                    transport_secret_key,
                    move |code| {
                        let _ = app_handle.emit("device-link-code", code);
                    },
                )
                .await;
                match &result {
                    Ok(_) => conn.close(VarInt::from_u32(LINK_CLOSE_DONE), b"done"),
                    Err(_) => conn.close(VarInt::from_u32(LINK_CLOSE_FAILED), b"link failed"),
                }
                result
            }
            Ok(Err(e)) => Err(e.into()),
            Err(e) => Err(e),
        };
        target.secret.zeroize();
        transport_secret_key.zeroize();
        result
    }
}

/// Run the new-device side of the link exchange on an established connection to
/// `responder`: send `secret`, the NodeId of `transport_secret_key` and the local device
/// name, pass the `link_code` to `show_code`, then read and validate the bundle.
async fn exchange_link(
    conn: &Connection,
    secret: &[u8; 32],
    responder: EndpointId,
    transport_secret_key: [u8; 32],
    show_code: impl FnOnce(String),
) -> Result<(IdentityKeys, LinkBundleData), AppError> {
    let (mut send, mut recv) =
        link_step("open stream", LINK_STEP_TIMEOUT, conn.open_bi()).await??;
    let new_node = SecretKey::from_bytes(&transport_secret_key).public();
    let mut encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
    let request = serde_json::to_vec(&PeerRequest::LinkRequest {
        secret: encoded.clone(),
        node_id: new_node.to_string(),
        device_name: local_device_name(),
    });
    encoded.zeroize();
    let mut request = request?;
    let written = link_step("write request", LINK_STEP_TIMEOUT, send.write_all(&request)).await;
    request.zeroize();
    written??;
    send.finish()?;
    show_code(link_code(&responder, &new_node));

    let read = link_step(
        "read bundle",
        LINK_CONFIRM_TIMEOUT + LINK_BUNDLE_TIMEOUT,
        recv.read_to_end(MAX_BUNDLE_LEN),
    )
    .await?;
    let mut bundle_json = match read {
        Ok(bundle_json) => bundle_json,
        Err(_) if was_declined(conn) => {
            return Err("The other device declined the link.".into());
        }
        Err(e) => return Err(e.into()),
    };
    let parsed: Result<LinkBundleData, _> = serde_json::from_slice(&bundle_json);
    bundle_json.zeroize();
    let mut bundle = parsed?;
    let keys = validate_link_bundle(&mut bundle, &responder.to_string(), transport_secret_key)?;
    log::info!("[link] received link bundle");
    Ok((keys, bundle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::loopback_endpoint;
    use proscenium_types::{
        DeviceEntry, LinkedDevicesAnnouncement, sign_linked_devices_announcement,
    };

    fn node_id(key: &[u8; 32]) -> String {
        SecretKey::from_bytes(key).public().to_string()
    }

    /// The device list `node_ids` of the identity `signer`, signed by `signer`.
    fn announcement(signer: &[u8; 32], node_ids: &[String]) -> LinkedDevicesAnnouncement {
        let key = SecretKey::from_bytes(signer);
        let mut announcement = LinkedDevicesAnnouncement {
            master_pubkey: key.public().to_string(),
            devices: node_ids
                .iter()
                .map(|node_id| DeviceEntry {
                    node_id: node_id.clone(),
                    device_name: "Linux".to_string(),
                    added_at: 1,
                })
                .collect(),
            removed: vec![],
            version: 1,
            signature: String::new(),
        };
        sign_linked_devices_announcement(&mut announcement, &key);
        announcement
    }

    /// A bundle with the master key `master` and the device list `node_ids` signed by it.
    fn bundle(master: &[u8; 32], node_ids: &[String]) -> LinkBundleData {
        LinkBundleData {
            master_secret_key: base64::engine::general_purpose::STANDARD.encode(master),
            announcement: announcement(master, node_ids),
            profile: None,
            follows: vec![],
            moderation: vec![],
            bookmarks: vec![],
            follow_requests: vec![],
            peer_announcements: vec![],
        }
    }

    fn encoded(secret: &[u8; 32]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
    }

    /// Existing-device side of one link exchange on `endpoint` with a session holding
    /// `secret`: the request read of `PeerHandler::accept`, then the steps of
    /// `serve_link_request` without storage and dialog. `confirm` is the answer of the
    /// user. Returns the link code the dialog shows.
    async fn serve_one_link(
        endpoint: Endpoint,
        secret: [u8; 32],
        bundle_json: Vec<u8>,
        confirm: bool,
    ) -> Result<String, AppError> {
        let pending: PendingLinkState = Arc::new(tokio::sync::Mutex::new(Some(PendingLink::new(
            secret,
            now_millis() + LINK_SESSION_TTL_MS,
        ))));
        let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
        let conn = incoming.await.map_err(|e| AppError::Other(e.to_string()))?;
        let (mut send, mut recv) = conn.accept_bi().await?;
        let request = recv.read_to_end(10_000).await?;
        let PeerRequest::LinkRequest {
            secret, node_id, ..
        } = serde_json::from_slice(&request)?
        else {
            return Err("unexpected request".into());
        };
        let checked = check_link_secret(&pending, conn.remote_id(), &secret).await;
        if checked.is_err() {
            conn.close(VarInt::from_u32(LINK_CLOSE_FAILED), b"link failed");
        }
        checked?;
        assert!(pending.lock().await.is_none());
        let code = link_code(&endpoint.id(), &parse_pubkey(&node_id)?);
        if !confirm {
            conn.close(VarInt::from_u32(LINK_CLOSE_DECLINED), b"link declined");
            return Ok(code);
        }
        send_link_bundle(&mut send, &bundle_json).await?;
        wait_for_completion(&conn).await?;
        Ok(code)
    }

    /// Two loopback endpoints: the existing device (master `EXISTING_MASTER`, serving one
    /// link exchange) and the new device, connected to it.
    struct LinkPair {
        existing: Endpoint,
        new: Endpoint,
        conn: Connection,
        server: tokio::task::JoinHandle<Result<String, AppError>>,
    }

    const EXISTING_MASTER: [u8; 32] = [7u8; 32];
    const EXISTING_TRANSPORT: [u8; 32] = [17u8; 32];
    const NEW_TRANSPORT: [u8; 32] = [18u8; 32];
    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// A bundle of `EXISTING_MASTER` that lists the existing device.
    fn existing_bundle() -> LinkBundleData {
        bundle(&EXISTING_MASTER, &[node_id(&EXISTING_TRANSPORT)])
    }

    /// Start the existing device with a session holding `secret`, whose user answers the
    /// link code with `confirm`, and connect the new device.
    async fn link_pair(secret: [u8; 32], bundle: &LinkBundleData, confirm: bool) -> LinkPair {
        let existing =
            loopback_endpoint(SecretKey::from_bytes(&EXISTING_TRANSPORT), PEER_ALPN).await;
        let new = loopback_endpoint(SecretKey::from_bytes(&NEW_TRANSPORT), PEER_ALPN).await;
        let server = tokio::spawn(serve_one_link(
            existing.clone(),
            secret,
            serde_json::to_vec(bundle).unwrap(),
            confirm,
        ));
        let conn = tokio::time::timeout(TEST_TIMEOUT, new.connect(existing.addr(), PEER_ALPN))
            .await
            .expect("connect timed out")
            .expect("connect");
        LinkPair {
            existing,
            new,
            conn,
            server,
        }
    }

    impl LinkPair {
        /// Run the new-device side of the exchange with `secret`. Returns its result and
        /// the link code it showed.
        async fn exchange(
            &self,
            secret: &[u8; 32],
        ) -> (
            Result<(IdentityKeys, LinkBundleData), AppError>,
            Option<String>,
        ) {
            let mut shown = None;
            let result = tokio::time::timeout(
                TEST_TIMEOUT,
                exchange_link(
                    &self.conn,
                    secret,
                    self.existing.id(),
                    NEW_TRANSPORT,
                    |code| shown = Some(code),
                ),
            )
            .await
            .expect("exchange timed out");
            (result, shown)
        }

        /// Close the connection with `code`, return the result of the existing-device
        /// task, and close both endpoints.
        async fn close(self, code: u32) -> Result<String, AppError> {
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
        let secret = [9u8; 32];
        let mut sent = existing_bundle();
        sent.follows = (0..2_000u32)
            .map(|i| proscenium_types::FollowEntry {
                pubkey: format!("{i:064}"),
                followed_at: 1,
                state: "active".to_string(),
                last_changed_at: 1,
            })
            .collect();

        let pair = link_pair(secret, &sent, true).await;
        let (exchanged, shown) = pair.exchange(&secret).await;
        let Ok((keys, received)) = exchanged else {
            panic!("exchange failed");
        };
        assert_eq!(keys.master_secret_key, EXISTING_MASTER);
        assert_eq!(keys.transport_secret_key, NEW_TRANSPORT);
        assert_eq!(received.follows.len(), 2_000);
        assert_eq!(received.announcement.devices.len(), 1);
        assert!(received.master_secret_key.is_empty());

        // The existing device saw the completion close, and both devices showed the
        // same code.
        let code = pair
            .close(LINK_CLOSE_DONE)
            .await
            .expect("existing device side");
        assert_eq!(shown.as_deref(), Some(code.as_str()));
    }

    #[tokio::test]
    async fn declined_link_code_sends_no_bundle() {
        let secret = [9u8; 32];
        let pair = link_pair(secret, &existing_bundle(), false).await;
        let (exchanged, shown) = pair.exchange(&secret).await;
        let Err(e) = exchanged else {
            panic!("a declined link must fail");
        };
        assert!(e.to_string().contains("declined"), "{e}");
        let code = pair.close(LINK_CLOSE_FAILED).await.unwrap();
        assert_eq!(shown.as_deref(), Some(code.as_str()));
    }

    #[tokio::test]
    async fn link_exchange_with_wrong_secret_fails_on_both_devices() {
        let pair = link_pair([1u8; 32], &existing_bundle(), true).await;
        assert!(pair.exchange(&[2u8; 32]).await.0.is_err());
        assert!(pair.close(LINK_CLOSE_FAILED).await.is_err());
    }

    #[tokio::test]
    async fn existing_device_requires_the_completion_close_code() {
        let secret = [9u8; 32];
        let pair = link_pair(secret, &existing_bundle(), true).await;
        assert!(pair.exchange(&secret).await.0.is_ok());
        // Code 0 is what a dropped connection sends.
        assert!(pair.close(0).await.is_err());
    }

    #[test]
    fn link_code_depends_on_both_node_ids() {
        let (existing, new, other) = (remote(1), remote(2), remote(3));
        let code = link_code(&existing, &new);
        assert_eq!(code, link_code(&existing, &new));
        assert_eq!(code.len(), 7);
        assert!(code.chars().enumerate().all(|(i, c)| if i == 3 {
            c == ' '
        } else {
            c.is_ascii_digit()
        }));
        assert_ne!(code, link_code(&existing, &other));
        assert_ne!(code, link_code(&other, &new));
        assert_ne!(code, link_code(&new, &existing));
    }

    fn remote(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[tokio::test]
    async fn a_wrong_secret_counts_as_an_attempt_and_keeps_the_session() {
        let secret = [9u8; 32];
        let pending: PendingLinkState = Arc::new(tokio::sync::Mutex::new(Some(PendingLink::new(
            secret,
            now_millis() + LINK_SESSION_TTL_MS,
        ))));
        let device = remote(1);
        assert!(
            check_link_secret(&pending, device, &encoded(&[8u8; 32]))
                .await
                .is_err()
        );
        assert!(
            check_link_secret(&pending, device, "not base64!")
                .await
                .is_err()
        );
        assert_eq!(pending.lock().await.as_ref().unwrap().attempts[&device], 2);
        assert!(
            check_link_secret(&pending, device, &encoded(&secret))
                .await
                .is_ok()
        );
        assert!(pending.lock().await.is_none());
        assert!(
            check_link_secret(&pending, device, &encoded(&secret))
                .await
                .is_err()
        );
    }

    #[test]
    fn attempts_are_limited_per_remote_and_keep_the_session_for_others() {
        let secret = [4u8; 32];
        let mut slot = Some(PendingLink::new(secret, 1_000));
        for _ in 0..MAX_LINK_ATTEMPTS {
            assert_eq!(
                begin_link_attempt(&mut slot, remote(1), 500).unwrap(),
                secret
            );
        }
        assert!(begin_link_attempt(&mut slot, remote(1), 500).is_err());
        assert!(slot.is_some());
        // Another device can still present the secret.
        assert_eq!(
            begin_link_attempt(&mut slot, remote(2), 500).unwrap(),
            secret
        );
        // The number of remotes is bounded.
        for seed in 3..(MAX_LINK_REMOTES as u8 + 1) {
            begin_link_attempt(&mut slot, remote(seed), 500).unwrap();
        }
        assert!(begin_link_attempt(&mut slot, remote(200), 500).is_err());
    }

    #[test]
    fn expired_session_is_removed() {
        let mut slot = Some(PendingLink::new([4u8; 32], 1_000));
        assert!(begin_link_attempt(&mut slot, remote(1), 1_001).is_err());
        assert!(slot.is_none());
    }

    #[test]
    fn session_is_consumed_once_and_only_by_its_secret() {
        let mut slot = Some(PendingLink::new([4u8; 32], 1_000));
        assert!(!consume_link_session(&mut slot, &[5u8; 32]));
        assert!(slot.is_some());
        assert!(consume_link_session(&mut slot, &[4u8; 32]));
        assert!(!consume_link_session(&mut slot, &[4u8; 32]));
    }

    #[test]
    fn link_payload_is_validated() {
        let good = LinkQrPayload {
            node_id: node_id(&[2u8; 32]),
            secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9u8; 32]),
            relay_url: Some("not a url".to_string()),
            expires_at: 2_000,
        };
        let target = parse_link_payload(&good, 1_000).unwrap();
        assert_eq!(target.secret, [9u8; 32]);
        assert_eq!(target.node_id.to_string(), good.node_id);
        assert_eq!(target.addr.relay_urls().count(), 0);

        assert!(parse_link_payload(&good, 2_000).is_err());
        let bad_node = LinkQrPayload {
            node_id: "aaaaaaa\u{20ac}".to_string(),
            ..good.clone()
        };
        assert!(parse_link_payload(&bad_node, 1_000).is_err());
        let short_secret = LinkQrPayload {
            secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9u8; 31]),
            ..good.clone()
        };
        assert!(parse_link_payload(&short_secret, 1_000).is_err());
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
        let responder = node_id(&[30u8; 32]);
        let transport = [31u8; 32];

        let mut ok = bundle(&master, &[responder.clone(), node_id(&[32u8; 32])]);
        let keys = validate_link_bundle(&mut ok, &responder, transport).unwrap();
        assert_eq!(keys.master_secret_key, master);
        assert_eq!(keys.transport_secret_key, transport);
        assert!(ok.master_secret_key.is_empty());
        assert_eq!(ok.announcement.devices.len(), 2);

        // A master key of the wrong length.
        let mut short = bundle(&master, std::slice::from_ref(&responder));
        short.master_secret_key = base64::engine::general_purpose::STANDARD.encode([3u8; 31]);
        assert!(validate_link_bundle(&mut short, &responder, transport).is_err());
        // The device list does not list the responder.
        let mut unlisted = bundle(&master, &[node_id(&[32u8; 32])]);
        assert!(validate_link_bundle(&mut unlisted, &responder, transport).is_err());
        // The device list is signed by another identity.
        let mut foreign = bundle(&master, std::slice::from_ref(&responder));
        foreign.announcement = announcement(&[4u8; 32], std::slice::from_ref(&responder));
        assert!(validate_link_bundle(&mut foreign, &responder, transport).is_err());
    }
}
