use crate::constants::OUTBOX_FLUSH_INTERVAL;
use crate::error::AppError;
use iroh::{EndpointAddr, EndpointId};
use proscenium_types::{
    DM_ALPN, DirectMessage, DmAck, DmMessage, DmPayload, EncryptedEnvelope, short_id,
};
use tauri::Emitter;
use tokio_util::sync::CancellationToken;

use super::DmHandler;

impl DmHandler {
    /// Spawn the outbox flush loop. Select on immediate notify (from send_dm on peer-offline)
    /// or the periodic fallback timer so no messages are stranded indefinitely.
    pub fn start_background(&self, token: CancellationToken) {
        let handler = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = handler.outbox_notify.notified() => {}
                    _ = tokio::time::sleep(OUTBOX_FLUSH_INTERVAL) => {}
                }
                handler.flush_all_outbox().await;
            }
            log::info!("[dm] outbox flush loop stopped");
        });
    }

    /// Attempt delivery of all queued outbox messages. Entries that succeed are removed;
    /// entries that fail are left for the next flush.
    async fn flush_all_outbox(&self) {
        let entries = match self.storage.get_all_outbox_messages().await {
            Ok(e) => e,
            Err(e) => {
                log::error!("[dm] failed to read outbox: {e}");
                return;
            }
        };
        for (id, peer_pubkey, envelope_json, message_id) in entries {
            let envelope: EncryptedEnvelope = match serde_json::from_str(&envelope_json) {
                Ok(e) => e,
                Err(e) => {
                    log::error!("[dm] invalid outbox entry {}: {e}", short_id(&id));
                    continue;
                }
            };
            match self.try_send_envelope(&peer_pubkey, &envelope).await {
                Ok(()) => {
                    log::info!(
                        "[dm] outbox delivered to {}, removing entry",
                        short_id(&peer_pubkey)
                    );
                    if let Err(e) = self.storage.remove_outbox_message(&id).await {
                        log::error!("[dm] failed to remove outbox entry {}: {e}", short_id(&id));
                    }
                    self.mark_delivered(&message_id).await;
                }
                Err(e) => {
                    log::debug!(
                        "[dm] outbox flush: {} still offline: {e}",
                        short_id(&peer_pubkey)
                    );
                }
            }
        }
    }

    /// Send a DM to a peer. Encrypts with Double Ratchet and sends over QUIC.
    /// If the peer is offline, queues to outbox.
    /// On successful delivery, marks the message as delivered and emits `dm-delivered`.
    pub async fn send_dm(&self, peer_pubkey: &str, message: DirectMessage) -> Result<(), AppError> {
        let message_id = message.id.clone();
        let payload = DmPayload::Message(message);
        let envelope = super::crypto::encrypt_and_save(self, peer_pubkey, &payload).await?;

        match self.try_send_envelope(peer_pubkey, &envelope).await {
            Ok(()) => {
                log::info!("[dm] sent message to {}", short_id(peer_pubkey));
                self.mark_delivered(&message_id).await;
            }
            Err(e) => {
                log::warn!(
                    "[dm] peer {} offline, queuing to outbox: {e}",
                    short_id(peer_pubkey)
                );
                let envelope_json = serde_json::to_string(&envelope)?;
                let id = crate::util::generate_id();
                self.storage
                    .insert_outbox_message(
                        &id,
                        peer_pubkey,
                        &envelope_json,
                        proscenium_types::now_millis(),
                        &message_id,
                    )
                    .await?;
                self.outbox_notify.notify_one();
            }
        }

        Ok(())
    }

    /// Mark a message as delivered in storage and notify the frontend.
    pub(super) async fn mark_delivered(&self, message_id: &str) {
        if let Err(e) = self.storage.mark_dm_delivered(message_id).await {
            log::error!(
                "[dm] failed to mark delivered {}: {e}",
                short_id(message_id)
            );
            return;
        }
        let _ = self.app_handle.emit(
            "dm-delivered",
            serde_json::json!({ "message_id": message_id }),
        );
    }

    /// Try to send an encrypted envelope to a peer over QUIC.
    async fn try_send_envelope(
        &self,
        peer_master_pubkey: &str,
        envelope: &EncryptedEnvelope,
    ) -> Result<(), AppError> {
        let node_ids = self
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

        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.endpoint.connect(addr, DM_ALPN),
        )
        .await
        .map_err(|_| AppError::Other("connection timeout".into()))??;

        let (mut send, mut recv) = conn.open_bi().await?;

        let bytes = serde_json::to_vec(&DmMessage::Envelope(envelope.clone()))?;
        send.write_all(&bytes).await?;
        send.finish()?;

        // Wait for ACK
        let ack_bytes =
            tokio::time::timeout(std::time::Duration::from_secs(5), recv.read_to_end(256))
                .await
                .map_err(|_| AppError::Other("ack timeout".into()))??;
        serde_json::from_slice::<DmAck>(&ack_bytes)?;

        conn.close(0u32.into(), b"done");
        Ok(())
    }

    /// Send a lightweight DM signal (typing, read receipt) without storing a message.
    pub async fn send_signal(&self, peer_pubkey: &str, payload: DmPayload) -> Result<(), AppError> {
        let envelope = super::crypto::encrypt_and_save(self, peer_pubkey, &payload).await?;
        self.try_send_envelope(peer_pubkey, &envelope).await?;
        Ok(())
    }
}
