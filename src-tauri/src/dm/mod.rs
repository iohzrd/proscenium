mod crypto;
mod delivery;
mod handshake;
mod incoming;

use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use proscenium_types::{
    DM_ALPN, DirectMessage, DmHandshake, DmMessage, is_valid_dm_pubkey, is_valid_id, short_id,
    validate_media_list,
};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, PoisonError};
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::{Notify, OwnedMutexGuard, mpsc};
use zeroize::Zeroizing;

/// Maximum byte length of `DirectMessage.content`.
pub(crate) const MAX_DM_CONTENT_LEN: usize = 10_000;

/// Maximum byte length of a JSON-serialized `DmPayload`. A message with
/// `MAX_DM_CONTENT_LEN` content bytes and ten attachments with 2,048-byte tickets is about
/// 40 KiB when no character needs a JSON escape sequence.
pub(crate) const MAX_DM_PAYLOAD_LEN: usize = 64 * 1024;

/// Maximum ciphertext length in an envelope: the payload plus the 16-byte
/// ChaCha20Poly1305 tag.
const MAX_DM_CIPHERTEXT_LEN: usize = MAX_DM_PAYLOAD_LEN + 16;

/// Maximum byte length of the frame the acceptor reads. The largest valid frame is a
/// `DmMessage::Envelope` whose ciphertext is `MAX_DM_CIPHERTEXT_LEN` bytes, encoded as
/// two hex digits per byte. The other JSON fields take about 260 bytes.
const MAX_DM_FRAME_LEN: usize = 2 * MAX_DM_CIPHERTEXT_LEN + 512;

/// Maximum byte length of the answer the initiator reads: a `DmHandshake::Response`
/// (about 130 bytes) or a `DmAck`.
const MAX_DM_ANSWER_LEN: usize = 512;

/// Initiator: time allowed to open the QUIC connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Initiator: time allowed, once connected, to send a handshake and read the response.
/// The responder may first query the initiator's identity (up to 15 s) and wait for the
/// session lock (up to `SESSION_LOCK_TIMEOUT`).
const HANDSHAKE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(25);

/// Initiator: time allowed, once connected, to send an envelope and read the `DmAck`.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Acceptor: time allowed for each of these steps: the peer opens the bi-stream, the
/// frame arrives, the answer is written, the peer closes the connection.
const ACCEPT_STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// Acceptor: time allowed to wait for the session lock of the connecting peer.
const SESSION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of entries in `SessionLocks` at which unused entries are removed.
const SESSION_LOCKS_PRUNE_LEN: usize = 256;

/// A call signal received via the DM ratchet channel.
#[derive(Debug, Clone)]
pub struct CallSignal {
    pub peer_pubkey: String,
    pub payload: proscenium_types::DmPayload,
}

/// One async mutex per peer DM pubkey. A task holds the mutex from loading a ratchet
/// session to storing it, so two tasks cannot derive the same message key.
#[derive(Default)]
struct SessionLocks {
    locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl SessionLocks {
    /// Wait for the mutex of the session with `peer_dm_pubkey`.
    /// The map mutex is released before the wait.
    async fn lock(&self, peer_dm_pubkey: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
            if locks.len() >= SESSION_LOCKS_PRUNE_LEN {
                // An entry referenced only by the map is neither held nor awaited.
                locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            }
            locks.entry(peer_dm_pubkey.to_string()).or_default().clone()
        };
        lock.lock_owned().await
    }
}

/// Copy of the local identity fields one DM operation needs. Taken once so the identity
/// lock is not held across storage or network calls.
struct DmKeys {
    master_pubkey: String,
    /// Hex-encoded `x25519_public`.
    dm_pubkey: String,
    x25519_private: Zeroizing<[u8; 32]>,
    x25519_public: [u8; 32],
    ratchet_storage_key: Zeroizing<[u8; 32]>,
}

/// A peer's DM key and transport NodeIds from the delegation cache.
struct PeerRoute {
    master_pubkey: String,
    /// Hex-encoded `x25519_public`.
    dm_pubkey: String,
    x25519_public: [u8; 32],
    /// Never empty. Delivery starts with the first NodeId.
    node_ids: Vec<EndpointId>,
}

#[derive(Clone)]
pub struct DmHandler {
    pub(crate) storage: Arc<Storage>,
    pub(crate) app_handle: AppHandle,
    pub(crate) endpoint: Endpoint,
    pub(crate) identity: SharedIdentity,
    pub(crate) outbox_notify: Arc<Notify>,
    /// Set by `request_outbox_flush`. The next flush pass then includes entries whose
    /// retry delay has not elapsed.
    outbox_ignore_delay: Arc<AtomicBool>,
    pub(crate) call_signal_tx: mpsc::UnboundedSender<CallSignal>,
    session_locks: Arc<SessionLocks>,
}

impl std::fmt::Debug for DmHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DmHandler").finish_non_exhaustive()
    }
}

impl DmHandler {
    pub fn new(
        storage: Arc<Storage>,
        app_handle: AppHandle,
        endpoint: Endpoint,
        identity: SharedIdentity,
    ) -> Self {
        let (call_signal_tx, _rx) = mpsc::unbounded_channel();
        Self {
            storage,
            app_handle,
            endpoint,
            identity,
            outbox_notify: Arc::new(Notify::new()),
            outbox_ignore_delay: Arc::new(AtomicBool::new(false)),
            call_signal_tx,
            session_locks: Arc::new(SessionLocks::default()),
        }
    }

    /// Take the call signal receiver. Must be called once during setup
    /// to wire call signals into the CallHandler.
    pub fn set_call_signal_tx(&mut self, tx: mpsc::UnboundedSender<CallSignal>) {
        self.call_signal_tx = tx;
    }

    async fn dm_keys(&self) -> DmKeys {
        let id = self.identity.read().await;
        DmKeys {
            master_pubkey: id.master_pubkey.clone(),
            dm_pubkey: id.dm_pubkey.clone(),
            x25519_private: Zeroizing::new(id.dm_x25519_private),
            x25519_public: id.dm_x25519_public,
            ratchet_storage_key: Zeroizing::new(id.ratchet_storage_key),
        }
    }

    /// Read the DM pubkey and the transport NodeIds of `peer_master_pubkey` from the
    /// delegation cache. Returns an error if either is missing or malformed.
    async fn resolve_peer(&self, peer_master_pubkey: &str) -> Result<PeerRoute, AppError> {
        let dm_pubkey = self
            .storage
            .get_peer_dm_pubkey(peer_master_pubkey)
            .await?
            .filter(|key| is_valid_dm_pubkey(key))
            .ok_or_else(|| {
                AppError::Other(format!(
                    "no DM pubkey cached for {}",
                    short_id(peer_master_pubkey)
                ))
            })?;
        let x25519_public = crypto::dm_pubkey_to_x25519(&dm_pubkey)?;
        let node_ids: Vec<EndpointId> = self
            .storage
            .get_peer_transport_node_ids(peer_master_pubkey)
            .await?
            .iter()
            .filter_map(|id| id.parse().ok())
            .collect();
        if node_ids.is_empty() {
            return Err(AppError::Other(format!(
                "no transport NodeId for {}",
                short_id(peer_master_pubkey)
            )));
        }
        Ok(PeerRoute {
            master_pubkey: peer_master_pubkey.to_string(),
            dm_pubkey,
            x25519_public,
            node_ids,
        })
    }

    /// Connect to `node` on the DM ALPN, write `frame` on one bi-stream and return the
    /// peer's answer (at most `MAX_DM_ANSWER_LEN` bytes). `answer_timeout` covers opening
    /// the stream, the write and the read. The connection is closed before returning.
    async fn exchange(
        &self,
        node: EndpointId,
        frame: &[u8],
        answer_timeout: Duration,
    ) -> Result<Vec<u8>, AppError> {
        let conn = tokio::time::timeout(
            CONNECT_TIMEOUT,
            self.endpoint.connect(EndpointAddr::from(node), DM_ALPN),
        )
        .await
        .map_err(|_| AppError::Other("connect timeout".into()))??;

        let answer = tokio::time::timeout(answer_timeout, async {
            let (mut send, mut recv) = conn.open_bi().await?;
            send.write_all(frame).await?;
            send.finish()?;
            Ok::<_, AppError>(recv.read_to_end(MAX_DM_ANSWER_LEN).await?)
        })
        .await
        .map_err(|_| AppError::Other("answer timeout".into()));
        conn.close(0u32.into(), b"done");
        answer?
    }

    /// Handle one incoming connection: read one `DmMessage`, process it, write the answer
    /// and wait for the peer to close the connection.
    async fn serve(&self, conn: &Connection, remote: EndpointId) -> Result<(), AppError> {
        let remote_str = remote.to_string();

        // Refuse a NodeId that is blocked or belongs to a blocked user before reading
        // anything. The DM key in the frame is checked again after it is authenticated.
        let remote_owner = self
            .storage
            .get_master_pubkey_for_transport(&remote_str)
            .await
            .unwrap_or_else(|| remote_str.clone());
        if self.storage.is_blocked(&remote_owner).await? {
            return Err(AppError::Other("peer is blocked".into()));
        }

        let (mut send, mut recv) = tokio::time::timeout(ACCEPT_STEP_TIMEOUT, conn.accept_bi())
            .await
            .map_err(|_| AppError::Other("no stream opened in time".into()))??;

        let frame = tokio::time::timeout(ACCEPT_STEP_TIMEOUT, recv.read_to_end(MAX_DM_FRAME_LEN))
            .await
            .map_err(|_| AppError::Other("frame not received in time".into()))??;

        let answer = match serde_json::from_slice::<DmMessage>(&frame)? {
            DmMessage::Handshake(DmHandshake::Init {
                noise_message,
                sender,
            }) => handshake::handle_handshake(self, remote, &sender, &noise_message).await?,
            DmMessage::Handshake(DmHandshake::Response { .. }) => {
                return Err(AppError::Other("unexpected handshake response".into()));
            }
            DmMessage::Envelope(envelope) => {
                serde_json::to_vec(&self.handle_encrypted_message(envelope).await?)?
            }
        };

        tokio::time::timeout(ACCEPT_STEP_TIMEOUT, send.write_all(&answer))
            .await
            .map_err(|_| AppError::Other("answer not written in time".into()))??;
        send.finish()?;

        // The initiator closes the connection after it read the answer. Returning before
        // that would drop the connection while the answer may still be untransmitted.
        let _ = tokio::time::timeout(ACCEPT_STEP_TIMEOUT, conn.closed()).await;
        Ok(())
    }
}

/// Check a `DirectMessage` against the limits both the sender and the receiver apply:
/// id format, content length, attachment list, `reply_to` format.
pub(crate) fn validate_direct_message(message: &DirectMessage) -> Result<(), String> {
    if !is_valid_id(&message.id) {
        return Err("invalid message id".to_string());
    }
    if message.content.len() > MAX_DM_CONTENT_LEN {
        return Err(format!(
            "message content too long: {} bytes (max {MAX_DM_CONTENT_LEN})",
            message.content.len()
        ));
    }
    validate_media_list(&message.media)?;
    if message
        .reply_to
        .as_deref()
        .is_some_and(|id| !is_valid_id(id))
    {
        return Err("invalid reply_to id".to_string());
    }
    Ok(())
}

impl ProtocolHandler for DmHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        log::debug!(
            "[dm] incoming connection from {}",
            short_id(&remote.to_string())
        );
        match self.serve(&conn, remote).await {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!(
                    "[dm] connection from {} ended with an error: {e}",
                    short_id(&remote.to_string())
                );
                conn.close(1u32.into(), b"error");
                Err(AcceptError::from_err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proscenium_types::{DmPayload, EncryptedEnvelope, MediaAttachment, RatchetHeaderWire};

    fn message() -> DirectMessage {
        DirectMessage {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            content: "hello".to_string(),
            timestamp: 1,
            media: vec![],
            reply_to: None,
        }
    }

    #[test]
    fn direct_message_validation() {
        assert!(validate_direct_message(&message()).is_ok());

        let mut bad_id = message();
        bad_id.id = "m1".to_string();
        assert!(validate_direct_message(&bad_id).is_err());

        let mut long = message();
        long.content = "a".repeat(MAX_DM_CONTENT_LEN + 1);
        assert!(validate_direct_message(&long).is_err());
        long.content.pop();
        assert!(validate_direct_message(&long).is_ok());

        let mut bad_reply = message();
        bad_reply.reply_to = Some("not an id".to_string());
        assert!(validate_direct_message(&bad_reply).is_err());

        let attachment = MediaAttachment {
            hash: "h".to_string(),
            mime_type: "image/png".to_string(),
            filename: "a.png".to_string(),
            size: 1,
            ticket: "t".to_string(),
        };
        let mut repeated_hash = message();
        repeated_hash.media = vec![attachment.clone(), attachment];
        assert!(validate_direct_message(&repeated_hash).is_err());
    }

    #[test]
    fn largest_envelope_fits_in_the_frame_limit() {
        let envelope = EncryptedEnvelope {
            sender: "f".repeat(64),
            ratchet_header: RatchetHeaderWire {
                dh_public: "f".repeat(64),
                message_number: u32::MAX,
                previous_chain_length: u32::MAX,
            },
            ciphertext: vec![0xff; MAX_DM_CIPHERTEXT_LEN],
        };
        let frame = serde_json::to_vec(&DmMessage::Envelope(envelope)).unwrap();
        assert!(frame.len() <= MAX_DM_FRAME_LEN);
    }

    #[test]
    fn unescaped_maximum_message_fits_in_the_payload_limit() {
        let media = (0..10)
            .map(|i| MediaAttachment {
                hash: format!("{i:064}"),
                mime_type: "m".repeat(127),
                filename: "f".repeat(255),
                size: u64::MAX,
                ticket: "t".repeat(2_048),
            })
            .collect();
        let payload = DmPayload::Message(DirectMessage {
            id: "f".repeat(32),
            content: "c".repeat(MAX_DM_CONTENT_LEN),
            timestamp: u64::MAX,
            media,
            reply_to: Some("f".repeat(32)),
        });
        assert!(serde_json::to_vec(&payload).unwrap().len() <= MAX_DM_PAYLOAD_LEN);
    }

    #[tokio::test]
    async fn session_locks_serialize_one_peer_and_prune_unused_entries() {
        let locks = Arc::new(SessionLocks::default());

        let guard = locks.lock("peer").await;
        let waiter = {
            let locks = locks.clone();
            tokio::spawn(async move { drop(locks.lock("peer").await) })
        };
        // A different peer is not blocked by the held mutex.
        drop(locks.lock("other").await);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        drop(guard);
        waiter.await.unwrap();

        // Unused entries are removed once the map reaches the prune length; the entry of
        // a held mutex stays.
        let held = locks.lock("held").await;
        for i in 0..2 * SESSION_LOCKS_PRUNE_LEN {
            drop(locks.lock(&format!("peer-{i}")).await);
        }
        let map = locks.locks.lock().unwrap();
        assert!(map.len() <= SESSION_LOCKS_PRUNE_LEN);
        assert!(map.contains_key("held"));
        drop(map);
        drop(held);
    }
}
