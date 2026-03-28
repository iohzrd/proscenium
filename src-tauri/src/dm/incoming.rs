use crate::error::AppError;
use crate::storage::Storage;
use proscenium_types::{DmPayload, EncryptedEnvelope, StoredMessage, now_millis, short_id};
use tauri::Emitter;

use super::CallSignal;
use super::DmHandler;
use super::crypto::{open_ratchet_state, seal_ratchet_state, wire_to_ratchet_header};

impl DmHandler {
    /// Handle an incoming encrypted message.
    /// `peer_dm_pubkey` is taken from `EncryptedEnvelope.sender`.
    pub(super) async fn handle_encrypted_message(
        &self,
        peer_dm_pubkey: &str,
        envelope: EncryptedEnvelope,
    ) -> Result<(), AppError> {
        log::debug!(
            "[dm] received envelope: sender={} msg_n={} prev_chain={} dh={} ciphertext_len={}",
            short_id(&envelope.sender),
            envelope.ratchet_header.message_number,
            envelope.ratchet_header.previous_chain_length,
            &envelope.ratchet_header.dh_public[..8],
            envelope.ciphertext.len(),
        );
        let (ratchet_storage_key, my_master_pubkey) = {
            let id = self.identity.read().await;
            (id.ratchet_storage_key, id.master_pubkey.clone())
        };

        // Load and decrypt ratchet session (keyed by DM pubkey)
        let stored = self
            .storage
            .get_ratchet_session(peer_dm_pubkey)
            .await?
            .ok_or_else(|| {
                AppError::Other(format!("no session with {}", short_id(peer_dm_pubkey)))
            })?;
        let json = open_ratchet_state(&ratchet_storage_key, &stored)?;
        let mut ratchet: crate::crypto::RatchetState = serde_json::from_str(&json)?;

        // Convert wire header to crypto header
        let header = wire_to_ratchet_header(&envelope.ratchet_header)?;

        // Decrypt
        let plaintext = ratchet.decrypt(&header, &envelope.ciphertext)?;

        // Serialize updated ratchet state (encrypted)
        let ratchet_json = serde_json::to_string(&ratchet)?;
        let ratchet_sealed = seal_ratchet_state(&ratchet_storage_key, &ratchet_json)?;
        let ratchet_ts = now_millis();

        // Parse payload
        let payload: DmPayload = serde_json::from_slice(&plaintext)?;

        match payload {
            DmPayload::Message(msg) => {
                // Master pubkey is required to route the message to the right conversation.
                let peer_master_pubkey = self
                    .storage
                    .get_master_pubkey_for_dm_pubkey(peer_dm_pubkey)
                    .await
                    .ok_or_else(|| {
                        AppError::Other(format!(
                            "no master pubkey for DM key {}",
                            short_id(peer_dm_pubkey)
                        ))
                    })?;

                let conv_id = Storage::conversation_id(&my_master_pubkey, &peer_master_pubkey);
                let preview = if msg.content.len() > 80 {
                    format!("{}...", &msg.content[..77])
                } else {
                    msg.content.clone()
                };

                let stored_msg = StoredMessage {
                    id: msg.id.clone(),
                    conversation_id: conv_id.clone(),
                    from_pubkey: peer_master_pubkey.clone(),
                    to_pubkey: my_master_pubkey.clone(),
                    content: msg.content,
                    timestamp: msg.timestamp,
                    media: msg.media,
                    read: false,
                    delivered: true,
                    reply_to: msg.reply_to,
                };

                // Atomically save ratchet state + store message (single SQLite transaction)
                self.storage
                    .receive_dm_message_atomically(
                        peer_dm_pubkey,
                        &peer_master_pubkey,
                        &ratchet_sealed,
                        ratchet_ts,
                        &stored_msg,
                        &preview,
                    )
                    .await?;

                log::info!(
                    "[dm] received message from {}",
                    short_id(&peer_master_pubkey)
                );

                let _ = self.app_handle.emit(
                    "dm-received",
                    serde_json::json!({
                        "from": peer_master_pubkey,
                        "message": stored_msg,
                    }),
                );
            }
            DmPayload::Delivered { message_id } => {
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, ratchet_ts)
                    .await?;
                self.storage.mark_dm_delivered(&message_id).await?;
                let _ = self.app_handle.emit(
                    "dm-delivered",
                    serde_json::json!({ "message_id": message_id }),
                );
            }
            DmPayload::Read { message_id } => {
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, ratchet_ts)
                    .await?;
                self.storage.mark_dm_read_by_id(&message_id).await?;
                let _ = self
                    .app_handle
                    .emit("dm-read", serde_json::json!({ "message_id": message_id }));
            }
            DmPayload::Typing => {
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, ratchet_ts)
                    .await?;
                if let Some(peer_master_pubkey) = self
                    .storage
                    .get_master_pubkey_for_dm_pubkey(peer_dm_pubkey)
                    .await
                {
                    let _ = self.app_handle.emit(
                        "typing-indicator",
                        serde_json::json!({ "peer": peer_master_pubkey }),
                    );
                }
            }
            payload @ (DmPayload::CallOffer { .. }
            | DmPayload::CallAnswer { .. }
            | DmPayload::CallReject { .. }
            | DmPayload::CallHangup { .. }) => {
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, ratchet_ts)
                    .await?;
                if let Some(peer_master_pubkey) = self
                    .storage
                    .get_master_pubkey_for_dm_pubkey(peer_dm_pubkey)
                    .await
                {
                    let _ = self.call_signal_tx.send(CallSignal {
                        peer_pubkey: peer_master_pubkey,
                        payload,
                    });
                }
            }
        }

        Ok(())
    }
}
