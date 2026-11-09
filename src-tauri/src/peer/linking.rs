//! Device linking over the peer ALPN.
//!
//! The existing device shows a QR code with its NodeId and a one-time secret. The new
//! device dials that NodeId, so QUIC authenticates the existing device; the secret
//! authenticates the new device.
//!
//! One bi-stream, opened by the new device:
//! - new device: JSON `PeerRequest::LinkRequest { secret }`, then finish.
//! - existing device: compares the secret in constant time and removes the session, then
//!   writes the `LinkBundleData` JSON and finishes.
//!
//! The new device validates the bundle and closes the connection with `LINK_CLOSE_DONE`.
//! The existing device stores the new device only after it received that close code.

use crate::error::AppError;
use crate::setup::IdentityKeys;
use crate::state::{device_index_of, device_node_id, own_devices};
use base64::Engine;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayUrl,
    endpoint::{Connection, ConnectionError, SendStream, VarInt},
    protocol::AcceptError,
};
use proscenium_types::{
    LinkBundleData, LinkQrPayload, MAX_LINKED_DEVICES, PEER_ALPN, PeerRequest, now_millis, short_id,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tauri::Emitter;
use zeroize::Zeroize;

use super::PeerHandler;

/// Device-link session on the existing device, created by `start_link_session`.
///
/// It is removed when a request presents its secret, when a request arrives after it
/// expired, when it is cancelled or replaced, or by the request that follows
/// `MAX_LINK_ATTEMPTS` attempts.
pub(super) struct PendingLink {
    secret: [u8; 32],
    expires_at: u64,
    attempts: u32,
}

pub(super) type PendingLinkState = Arc<tokio::sync::Mutex<Option<PendingLink>>>;

pub(super) const LINK_SESSION_TTL_MS: u64 = 60_000; // 60 seconds
/// Link requests served per session. A request with a wrong secret counts as one.
const MAX_LINK_ATTEMPTS: u32 = 5;
/// Time limit for each network step of the link exchange except the bundle transfer.
const LINK_STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// Time limit for writing or reading the link bundle.
const LINK_BUNDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Largest accepted `LinkBundleData` JSON.
const MAX_BUNDLE_LEN: usize = 16 * 1024 * 1024;

/// QUIC close code sent by the new device after it validated the bundle.
/// Not 0: dropping a connection without `close` sends code 0.
const LINK_CLOSE_DONE: u32 = 1;
/// QUIC close code sent by either device when the exchange failed.
const LINK_CLOSE_FAILED: u32 = 2;

/// Count one link attempt against the session in `slot` and return its secret.
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

/// Lowest transport key derivation index in `1..MAX_LINKED_DEVICES` whose NodeId is not
/// in `used_node_ids`. `None` if every index is in use.
fn lowest_unused_device_index(
    master_secret_key_bytes: &[u8; 32],
    used_node_ids: &HashSet<String>,
) -> Option<u32> {
    (1..MAX_LINKED_DEVICES as u32)
        .find(|index| !used_node_ids.contains(&device_node_id(master_secret_key_bytes, *index)))
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

/// Check a link bundle received from the device `responder_node_id` and return
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

/// Existing-device side: check that `presented` equals the session secret and remove
/// the session. The comparison takes the same time for every presented value.
async fn check_link_secret(pending: &PendingLinkState, presented: &str) -> Result<(), AppError> {
    let secret = {
        let mut slot = pending.lock().await;
        begin_link_attempt(&mut slot, now_millis())?
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

        *self.pending_link.lock().await = Some(PendingLink {
            secret,
            expires_at,
            attempts: 0,
        });

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
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        let mut secret = secret;
        let served = self.serve_link_request(send, &secret, conn).await;
        secret.zeroize();
        match served {
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

    /// Serve one link request: check the secret and remove the session, send the
    /// bundle, and store and announce the new device after the new device closed the
    /// connection with `LINK_CLOSE_DONE`.
    async fn serve_link_request(
        &self,
        mut send: SendStream,
        secret: &str,
        conn: &Connection,
    ) -> Result<(), AppError> {
        check_link_secret(&self.pending_link, secret).await?;

        let (master_secret_key_bytes, master_pubkey, device_index) = {
            let id = self.identity.read().await;
            (
                id.master_secret_key_bytes,
                id.master_pubkey.clone(),
                id.device_index,
            )
        };

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
        let sent = send_link_bundle(&mut send, &bundle_json).await;
        let bundle_len = bundle_json.len();
        bundle_json.zeroize();
        sent?;
        log::info!("[link] sent link bundle ({bundle_len} bytes)");

        wait_for_completion(conn).await?;

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
    /// Connects to the device named in `payload`, sends the secret, and receives and
    /// validates the link bundle. Returns the keys of the identity to install and the
    /// bundle (with `master_secret_key` cleared). Writes nothing to disk.
    pub async fn request_link(
        &self,
        endpoint: &Endpoint,
        payload: &LinkQrPayload,
    ) -> Result<(IdentityKeys, LinkBundleData), AppError> {
        let mut target = parse_link_payload(payload, now_millis())?;
        let responder_node_id = target.node_id.to_string();
        if self
            .identity
            .read()
            .await
            .device_index_of(&responder_node_id)
            .is_some()
        {
            target.secret.zeroize();
            return Err("this device already belongs to the identity of that device".into());
        }

        // QUIC authenticates the NodeId of the link code.
        let connected = link_step(
            "connect",
            LINK_STEP_TIMEOUT,
            endpoint.connect(target.addr, PEER_ALPN),
        )
        .await;
        let result = match connected {
            Ok(Ok(conn)) => {
                let result = exchange_link(&conn, &target.secret, &responder_node_id).await;
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
        result
    }
}

/// Run the new-device side of the link exchange on an established connection: send
/// `secret`, then read and validate the bundle.
async fn exchange_link(
    conn: &Connection,
    secret: &[u8; 32],
    responder_node_id: &str,
) -> Result<(IdentityKeys, LinkBundleData), AppError> {
    let (mut send, mut recv) =
        link_step("open stream", LINK_STEP_TIMEOUT, conn.open_bi()).await??;
    let mut encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
    let request = serde_json::to_vec(&PeerRequest::LinkRequest {
        secret: encoded.clone(),
    });
    encoded.zeroize();
    let mut request = request?;
    let written = link_step("write request", LINK_STEP_TIMEOUT, send.write_all(&request)).await;
    request.zeroize();
    written??;
    send.finish()?;

    let mut bundle_json = link_step(
        "read bundle",
        LINK_BUNDLE_TIMEOUT,
        recv.read_to_end(MAX_BUNDLE_LEN),
    )
    .await??;
    let parsed: Result<LinkBundleData, _> = serde_json::from_slice(&bundle_json);
    bundle_json.zeroize();
    let mut bundle = parsed?;
    let keys = validate_link_bundle(&mut bundle, responder_node_id)?;
    log::info!(
        "[link] received link bundle (device index {})",
        keys.device_index
    );
    Ok((keys, bundle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::loopback_endpoint;
    use proscenium_types::{DeviceEntry, derive_transport_key};

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

    fn encoded(secret: &[u8; 32]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
    }

    /// Existing-device side of one link exchange on `endpoint` with a session holding
    /// `secret`: the request read of `PeerHandler::accept`, then the steps of
    /// `serve_link_request` without storage.
    async fn serve_one_link(
        endpoint: Endpoint,
        secret: [u8; 32],
        bundle_json: Vec<u8>,
    ) -> Result<(), AppError> {
        let pending: PendingLinkState = Arc::new(tokio::sync::Mutex::new(Some(PendingLink {
            secret,
            expires_at: now_millis() + LINK_SESSION_TTL_MS,
            attempts: 0,
        })));
        let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
        let conn = incoming.await.map_err(|e| AppError::Other(e.to_string()))?;
        let (mut send, mut recv) = conn.accept_bi().await?;
        let request = recv.read_to_end(10_000).await?;
        let PeerRequest::LinkRequest { secret } = serde_json::from_slice(&request)? else {
            return Err("unexpected request".into());
        };
        let checked = check_link_secret(&pending, &secret).await;
        if checked.is_err() {
            conn.close(VarInt::from_u32(LINK_CLOSE_FAILED), b"link failed");
        }
        checked?;
        assert!(pending.lock().await.is_none());
        send_link_bundle(&mut send, &bundle_json).await?;
        wait_for_completion(&conn).await
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

    /// Start the existing device with a session holding `secret` and connect the new
    /// device.
    async fn link_pair(secret: [u8; 32], bundle: &LinkBundleData) -> LinkPair {
        let existing = loopback_endpoint(
            iroh::SecretKey::from_bytes(&derive_transport_key(&EXISTING_MASTER, 0)),
            PEER_ALPN,
        )
        .await;
        let new = loopback_endpoint(
            iroh::SecretKey::from_bytes(&derive_transport_key(&[8u8; 32], 0)),
            PEER_ALPN,
        )
        .await;
        let server = tokio::spawn(serve_one_link(
            existing.clone(),
            secret,
            serde_json::to_vec(bundle).unwrap(),
        ));
        let conn = tokio::time::timeout(TEST_TIMEOUT, new.connect(existing.addr(), PEER_ALPN))
            .await
            .expect("connect timed out")
            .expect("connect");
        LinkPair {
            responder_node_id: existing.id().to_string(),
            existing,
            new,
            conn,
            server,
        }
    }

    impl LinkPair {
        /// Run the new-device side of the exchange with `secret`.
        async fn exchange(
            &self,
            secret: &[u8; 32],
        ) -> Result<(IdentityKeys, LinkBundleData), AppError> {
            tokio::time::timeout(
                TEST_TIMEOUT,
                exchange_link(&self.conn, secret, &self.responder_node_id),
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
        let secret = [9u8; 32];
        let mut sent = bundle(&EXISTING_MASTER, 1, vec![device(&EXISTING_MASTER, 0)]);
        sent.follows = (0..2_000u32)
            .map(|i| proscenium_types::FollowEntry {
                pubkey: format!("{i:064}"),
                followed_at: 1,
                state: "active".to_string(),
                last_changed_at: 1,
            })
            .collect();

        let pair = link_pair(secret, &sent).await;
        let Ok((keys, received)) = pair.exchange(&secret).await else {
            panic!("exchange failed");
        };
        assert_eq!(keys.master_secret_key, EXISTING_MASTER);
        assert_eq!(keys.device_index, 1);
        assert_eq!(received.follows.len(), 2_000);
        assert_eq!(received.devices.len(), 1);
        assert!(received.master_secret_key.is_empty());

        // The existing device saw the completion close.
        pair.close(LINK_CLOSE_DONE)
            .await
            .expect("existing device side");
    }

    #[tokio::test]
    async fn link_exchange_with_wrong_secret_fails_on_both_devices() {
        let sent = bundle(&EXISTING_MASTER, 1, vec![device(&EXISTING_MASTER, 0)]);
        let pair = link_pair([1u8; 32], &sent).await;
        assert!(pair.exchange(&[2u8; 32]).await.is_err());
        assert!(pair.close(LINK_CLOSE_FAILED).await.is_err());
    }

    #[tokio::test]
    async fn existing_device_requires_the_completion_close_code() {
        let secret = [9u8; 32];
        let sent = bundle(&EXISTING_MASTER, 1, vec![device(&EXISTING_MASTER, 0)]);
        let pair = link_pair(secret, &sent).await;
        assert!(pair.exchange(&secret).await.is_ok());
        // Code 0 is what a dropped connection sends.
        assert!(pair.close(0).await.is_err());
    }

    #[tokio::test]
    async fn a_wrong_secret_counts_as_an_attempt_and_keeps_the_session() {
        let secret = [9u8; 32];
        let pending: PendingLinkState = Arc::new(tokio::sync::Mutex::new(Some(PendingLink {
            secret,
            expires_at: now_millis() + LINK_SESSION_TTL_MS,
            attempts: 0,
        })));
        assert!(
            check_link_secret(&pending, &encoded(&[8u8; 32]))
                .await
                .is_err()
        );
        assert!(check_link_secret(&pending, "not base64!").await.is_err());
        assert_eq!(pending.lock().await.as_ref().unwrap().attempts, 2);
        assert!(check_link_secret(&pending, &encoded(&secret)).await.is_ok());
        assert!(pending.lock().await.is_none());
        assert!(
            check_link_secret(&pending, &encoded(&secret))
                .await
                .is_err()
        );
    }

    #[test]
    fn session_allows_five_attempts_and_survives_failed_ones() {
        let secret = [4u8; 32];
        let mut slot = Some(PendingLink {
            secret,
            expires_at: 1_000,
            attempts: 0,
        });
        for _ in 0..MAX_LINK_ATTEMPTS {
            assert_eq!(begin_link_attempt(&mut slot, 500).unwrap(), secret);
            assert!(slot.is_some());
        }
        assert!(begin_link_attempt(&mut slot, 500).is_err());
        assert!(slot.is_none());
        assert!(begin_link_attempt(&mut slot, 500).is_err());
    }

    #[test]
    fn expired_session_is_removed() {
        let mut slot = Some(PendingLink {
            secret: [4u8; 32],
            expires_at: 1_000,
            attempts: 0,
        });
        assert!(begin_link_attempt(&mut slot, 1_001).is_err());
        assert!(slot.is_none());
    }

    #[test]
    fn session_is_consumed_once_and_only_by_its_secret() {
        let mut slot = Some(PendingLink {
            secret: [4u8; 32],
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
}
