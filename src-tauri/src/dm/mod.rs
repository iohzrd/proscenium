mod crypto;
mod delivery;
mod handshake;
mod incoming;

use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::{
    Endpoint,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use proscenium_types::{DmAck, DmHandshake, DmMessage, short_id};
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::{Notify, mpsc};

/// A call signal received via the DM ratchet channel.
#[derive(Debug, Clone)]
pub struct CallSignal {
    pub peer_pubkey: String,
    pub payload: proscenium_types::DmPayload,
}

#[derive(Debug, Clone)]
pub struct DmHandler {
    pub(crate) storage: Arc<Storage>,
    pub(crate) app_handle: AppHandle,
    pub(crate) endpoint: Endpoint,
    pub(crate) identity: SharedIdentity,
    pub(crate) outbox_notify: Arc<Notify>,
    pub(crate) call_signal_tx: mpsc::UnboundedSender<CallSignal>,
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
            call_signal_tx,
        }
    }

    /// Take the call signal receiver. Must be called once during setup
    /// to wire call signals into the CallHandler.
    pub fn set_call_signal_tx(&mut self, tx: mpsc::UnboundedSender<CallSignal>) {
        self.call_signal_tx = tx;
    }
}

impl ProtocolHandler for DmHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        let remote_str = remote.to_string();
        log::info!("[dm] incoming connection from {}", short_id(&remote_str));

        // Resolve transport NodeId to master pubkey for block check
        let remote_pubkey = self
            .storage
            .get_master_pubkey_for_transport(&remote_str)
            .await
            .unwrap_or_else(|| remote_str.clone());

        // Reject blocked peers
        if self
            .storage
            .is_blocked(&remote_pubkey)
            .await
            .unwrap_or(false)
        {
            log::warn!("[dm] rejecting blocked peer {}", short_id(&remote_pubkey));
            return Err(AcceptError::from_err(std::io::Error::other("blocked")));
        }

        let (mut send, mut recv) = conn.accept_bi().await?;

        let frame_bytes = recv
            .read_to_end(1_048_576)
            .await
            .map_err(AcceptError::from_err)?;

        let msg: DmMessage = serde_json::from_slice(&frame_bytes).map_err(AcceptError::from_err)?;

        match msg {
            DmMessage::Handshake(DmHandshake::Init {
                noise_message,
                sender,
            }) => {
                let response = handshake::handle_handshake(self, &sender, noise_message)
                    .await
                    .map_err(AcceptError::from_err)?;
                send.write_all(&response)
                    .await
                    .map_err(AcceptError::from_err)?;
                send.finish().map_err(AcceptError::from_err)?;
            }
            DmMessage::Handshake(DmHandshake::Response { .. }) => {
                log::error!(
                    "[dm] unexpected handshake response from {}",
                    short_id(&remote_str)
                );
            }
            DmMessage::Envelope(envelope) => {
                let sender = envelope.sender.clone();
                if let Err(e) = self.handle_encrypted_message(&sender, envelope).await {
                    log::error!(
                        "[dm] failed to handle message from {}: {e}",
                        short_id(&remote_str)
                    );
                }
                let ack = serde_json::to_vec(&DmAck).map_err(AcceptError::from_err)?;
                send.write_all(&ack).await.map_err(AcceptError::from_err)?;
                send.finish().map_err(AcceptError::from_err)?;
            }
        }

        conn.closed().await;
        Ok(())
    }
}
