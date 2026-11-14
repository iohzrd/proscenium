//! Direct messages over the DM ALPN.
//!
//! Every payload is sent in a `DmEnvelope` to one device of the recipient on its own QUIC
//! connection, which encrypts it and authenticates both devices. A message, receipt or
//! typing signal goes to every device of the recipient; a call signal goes to one device.
//! There is no session state.

mod delivery;
mod incoming;

use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use crate::util::{NodeSlot, RateWindow};
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use proscenium_types::{
    DM_ALPN, DirectMessage, DmEnvelope, MAX_ANNOUNCEMENT_JSON_LEN, is_valid_id, short_id,
    validate_media_list,
};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tokio::sync::{Notify, Semaphore, mpsc};

/// Maximum byte length of `DirectMessage.content`.
pub(crate) const MAX_DM_CONTENT_LEN: usize = 10_000;

/// Maximum byte length of a JSON-serialized `DmPayload`. A message with
/// `MAX_DM_CONTENT_LEN` content bytes and ten attachments with 2,048-byte tickets is about
/// 40 KiB when no character needs a JSON escape sequence.
pub(crate) const MAX_DM_PAYLOAD_LEN: usize = 64 * 1024;

/// Maximum byte length of the envelope the acceptor reads: a payload of
/// `MAX_DM_PAYLOAD_LEN` bytes and a device list of at most `MAX_ANNOUNCEMENT_JSON_LEN`
/// bytes. The other fields take under 1 KiB.
const MAX_DM_FRAME_LEN: usize = MAX_DM_PAYLOAD_LEN + MAX_ANNOUNCEMENT_JSON_LEN + 1024;

/// New conversations with strangers (see `Storage::receive_dm_message`) accepted per
/// `STRANGER_WINDOW`, over all senders. NodeIds and identities cost nothing to generate,
/// so a per-sender limit does not stop one sender with many keys from replacing every
/// stranger conversation; this limit makes that take hours.
const MAX_NEW_STRANGER_CONVERSATIONS: usize = 20;
const STRANGER_WINDOW: Duration = Duration::from_secs(60 * 60);

/// New stranger conversations per `STRANGER_WINDOW` whose first message is reported with
/// `notify` set, so the frontend shows an OS notification. Later messages of a stranger
/// set no `notify`.
const MAX_STRANGER_NOTIFICATIONS: usize = 3;

/// Incoming DM connections handled at the same time: from devices of contacts (the
/// NodeId is in the cached device list of a user who is no stranger, see
/// `Storage::is_stranger`), from other NodeIds, and per remote NodeId. NodeIds cost
/// nothing to generate, so connections of other NodeIds cannot take the slots of
/// contacts.
const DM_MAX_CONNECTIONS: usize = 64;
const DM_MAX_UNKNOWN_CONNECTIONS: usize = 16;
const DM_MAX_CONNECTIONS_PER_NODE: usize = 4;

/// Maximum byte length of the answer the sender reads (a `DmAck`).
const MAX_DM_ANSWER_LEN: usize = 64;

/// Sender: time allowed to open the QUIC connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Sender: time allowed, once connected, to send an envelope and read the `DmAck`.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Acceptor: time allowed for each of these steps: the peer opens the bi-stream, the
/// envelope arrives, the answer is written, the peer closes the connection.
const ACCEPT_STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// A call signal received from the device `peer_device` of `peer_pubkey`.
#[derive(Debug, Clone)]
pub struct CallSignal {
    pub peer_pubkey: String,
    pub peer_device: EndpointId,
    pub payload: proscenium_types::DmPayload,
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
    /// Limits the incoming connections from devices of contacts handled at the same time.
    connection_limit: Arc<Semaphore>,
    /// Limits the incoming connections from other NodeIds handled at the same time.
    unknown_connection_limit: Arc<Semaphore>,
    /// Incoming connections currently handled per remote NodeId.
    connections_per_node: Arc<StdMutex<HashMap<EndpointId, usize>>>,
    /// New stranger conversations accepted within `STRANGER_WINDOW`.
    stranger_admissions: Arc<StdMutex<RateWindow>>,
    /// New stranger conversations reported with `notify` within `STRANGER_WINDOW`.
    stranger_notifications: Arc<StdMutex<RateWindow>>,
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
            connection_limit: Arc::new(Semaphore::new(DM_MAX_CONNECTIONS)),
            unknown_connection_limit: Arc::new(Semaphore::new(DM_MAX_UNKNOWN_CONNECTIONS)),
            connections_per_node: Arc::new(StdMutex::new(HashMap::new())),
            stranger_admissions: Arc::new(StdMutex::new(RateWindow::new(
                MAX_NEW_STRANGER_CONVERSATIONS,
                STRANGER_WINDOW,
            ))),
            stranger_notifications: Arc::new(StdMutex::new(RateWindow::new(
                MAX_STRANGER_NOTIFICATIONS,
                STRANGER_WINDOW,
            ))),
        }
    }

    /// Count one new stranger conversation against `MAX_NEW_STRANGER_CONVERSATIONS`.
    /// Returns false, and counts nothing, if the limit is reached.
    pub(super) fn admit_new_stranger(&self) -> bool {
        let Ok(mut admissions) = self.stranger_admissions.lock() else {
            return false;
        };
        admissions.admit(Instant::now())
    }

    /// Count one notification of a new stranger conversation against
    /// `MAX_STRANGER_NOTIFICATIONS`. Returns false, and counts nothing, if the limit is
    /// reached.
    pub(super) fn admit_stranger_notification(&self) -> bool {
        let Ok(mut notifications) = self.stranger_notifications.lock() else {
            return false;
        };
        notifications.admit(Instant::now())
    }

    /// Take the call signal receiver. Must be called once during setup
    /// to wire call signals into the CallHandler.
    pub fn set_call_signal_tx(&mut self, tx: mpsc::UnboundedSender<CallSignal>) {
        self.call_signal_tx = tx;
    }

    /// NodeIds of the cached device list of `peer_master_pubkey`, in list order.
    /// Returns an error if none is cached.
    async fn peer_devices(&self, peer_master_pubkey: &str) -> Result<Vec<EndpointId>, AppError> {
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
        Ok(node_ids)
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

    /// Handle one incoming connection from `remote`, whose user is `remote_owner` (the
    /// NodeId itself if no cached device list lists it): read one `DmEnvelope`, process
    /// it, write the answer and wait for the peer to close the connection.
    async fn serve(
        &self,
        conn: &Connection,
        remote: EndpointId,
        remote_owner: &str,
    ) -> Result<(), AppError> {
        // Refuse a NodeId that is blocked or belongs to a blocked user before reading
        // anything. The sender of the envelope is checked again after it is verified.
        if self.storage.is_blocked(remote_owner).await? {
            return Err(AppError::Other("peer is blocked".into()));
        }

        let (mut send, mut recv) = tokio::time::timeout(ACCEPT_STEP_TIMEOUT, conn.accept_bi())
            .await
            .map_err(|_| AppError::Other("no stream opened in time".into()))??;

        let frame = tokio::time::timeout(ACCEPT_STEP_TIMEOUT, recv.read_to_end(MAX_DM_FRAME_LEN))
            .await
            .map_err(|_| AppError::Other("envelope not received in time".into()))??;

        let envelope: DmEnvelope = serde_json::from_slice(&frame)?;
        let answer = serde_json::to_vec(&self.handle_envelope(remote, envelope).await?)?;

        tokio::time::timeout(ACCEPT_STEP_TIMEOUT, send.write_all(&answer))
            .await
            .map_err(|_| AppError::Other("answer not written in time".into()))??;
        send.finish()?;

        // The sender closes the connection after it read the answer. Returning before
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
    /// Serve one incoming envelope. At most `DM_MAX_CONNECTIONS` connections from devices
    /// of contacts and `DM_MAX_UNKNOWN_CONNECTIONS` from other NodeIds are handled at once,
    /// and `DM_MAX_CONNECTIONS_PER_NODE` per remote NodeId; others are closed.
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        let remote_str = remote.to_string();
        log::debug!("[dm] incoming connection from {}", short_id(&remote_str));
        let owner = self
            .storage
            .get_master_pubkey_for_transport(&remote_str)
            .await;
        let contact = match &owner {
            Some(master) => {
                let me = self.identity.read().await.master_pubkey.clone();
                !self
                    .storage
                    .is_stranger(&me, master)
                    .await
                    .map_err(AcceptError::from_err)?
            }
            None => false,
        };
        let limit = if contact {
            &self.connection_limit
        } else {
            &self.unknown_connection_limit
        };
        let Ok(_permit) = limit.clone().try_acquire_owned() else {
            conn.close(1u32.into(), b"busy");
            return Err(AcceptError::from_err(std::io::Error::other(
                "DM connection limit reached",
            )));
        };
        let Some(_node_slot) = NodeSlot::acquire(
            &self.connections_per_node,
            remote,
            DM_MAX_CONNECTIONS_PER_NODE,
        ) else {
            conn.close(1u32.into(), b"busy");
            return Err(AcceptError::from_err(std::io::Error::other(
                "per-node DM connection limit reached",
            )));
        };
        let owner = owner.unwrap_or_else(|| remote_str.clone());
        match self.serve(&conn, remote, &owner).await {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!(
                    "[dm] connection from {} ended with an error: {e}",
                    short_id(&remote_str)
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
    use iroh::SecretKey;
    use proscenium_types::{
        DeviceEntry, DmPayload, LinkedDevicesAnnouncement, MAX_DEVICE_NAME_LEN, MAX_LINKED_DEVICES,
        MAX_REMOVED_DEVICES, MediaAttachment,
    };

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

    /// A message whose JSON has `MAX_DM_CONTENT_LEN` content bytes and ten attachments
    /// with the largest fields, none of which needs a JSON escape sequence.
    fn maximum_message() -> DmPayload {
        let media = (0..10)
            .map(|i| MediaAttachment {
                hash: format!("{i:064}"),
                mime_type: "m".repeat(127),
                filename: "f".repeat(255),
                size: u64::MAX,
                ticket: "t".repeat(2_048),
            })
            .collect();
        DmPayload::Message(DirectMessage {
            id: "f".repeat(32),
            content: "c".repeat(MAX_DM_CONTENT_LEN),
            timestamp: u64::MAX,
            media,
            reply_to: Some("f".repeat(32)),
        })
    }

    #[test]
    fn largest_envelope_fits_in_the_frame_limit() {
        let key = |seed: u8| SecretKey::from_bytes(&[seed; 32]).public().to_string();
        let envelope = DmEnvelope {
            recipient: key(3),
            sender_devices: LinkedDevicesAnnouncement {
                master_pubkey: key(1),
                devices: (0..MAX_LINKED_DEVICES as u8)
                    .map(|i| DeviceEntry {
                        node_id: key(i + 10),
                        device_name: "\u{1}".repeat(MAX_DEVICE_NAME_LEN),
                        added_at: u64::MAX,
                    })
                    .collect(),
                removed: (0..MAX_REMOVED_DEVICES as u8)
                    .map(|i| key(i + 100))
                    .collect(),
                version: u64::MAX,
                signature: "f".repeat(128),
            },
            payload: maximum_message(),
        };
        let frame = serde_json::to_vec(&envelope).unwrap();
        assert!(frame.len() <= MAX_DM_FRAME_LEN);
    }

    #[test]
    fn unescaped_maximum_message_fits_in_the_payload_limit() {
        assert!(serde_json::to_vec(&maximum_message()).unwrap().len() <= MAX_DM_PAYLOAD_LEN);
    }
}
