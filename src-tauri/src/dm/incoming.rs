use crate::crypto::CryptoError;
use crate::error::AppError;
use crate::storage::Storage;
use proscenium_types::{
    DmAck, DmNack, DmPayload, EncryptedEnvelope, StoredMessage, is_valid_dm_pubkey, is_valid_id,
    now_millis, short_id,
};
use tauri::Emitter;

use super::crypto::{dm_pubkey_to_x25519, seal_session, wire_to_ratchet_header};
use super::{
    CallSignal, DmHandler, MAX_DM_CIPHERTEXT_LEN, SESSION_LOCK_TIMEOUT, validate_direct_message,
};

/// Check a decrypted payload: the message limits of `validate_direct_message`, and the
/// id format of every message id and call id.
fn validate_payload(payload: &DmPayload) -> Result<(), String> {
    match payload {
        DmPayload::Message(message) => validate_direct_message(message),
        DmPayload::Typing => Ok(()),
        DmPayload::Read { message_id } | DmPayload::Delivered { message_id } => {
            if is_valid_id(message_id) {
                Ok(())
            } else {
                Err("invalid message id in receipt".to_string())
            }
        }
        DmPayload::CallOffer { call_id, .. }
        | DmPayload::CallAnswer { call_id }
        | DmPayload::CallReject { call_id }
        | DmPayload::CallHangup { call_id } => {
            if is_valid_id(call_id) {
                Ok(())
            } else {
                Err("invalid call id".to_string())
            }
        }
    }
}

impl DmHandler {
    /// Handle an incoming envelope and return the acknowledgement to send.
    ///
    /// `Ok(DmAck::Failed(_))` means the envelope was refused for a reason the sender can
    /// act on. `Err` means a local failure (storage, lock timeout); no acknowledgement is
    /// sent, so the sender keeps the message queued.
    pub(super) async fn handle_encrypted_message(
        &self,
        envelope: EncryptedEnvelope,
    ) -> Result<DmAck, AppError> {
        let peer_dm_pubkey = envelope.sender.as_str();
        if !is_valid_dm_pubkey(peer_dm_pubkey) || envelope.ciphertext.len() > MAX_DM_CIPHERTEXT_LEN
        {
            return Ok(DmAck::Failed(DmNack::Rejected));
        }
        let (Ok(header), Ok(peer_x25519_public)) = (
            wire_to_ratchet_header(&envelope.ratchet_header),
            dm_pubkey_to_x25519(peer_dm_pubkey),
        ) else {
            return Ok(DmAck::Failed(DmNack::Rejected));
        };
        log::debug!(
            "[dm] received envelope: sender={} msg_n={} prev_chain={} ciphertext_len={}",
            short_id(peer_dm_pubkey),
            header.message_number,
            header.previous_chain_length,
            envelope.ciphertext.len(),
        );

        let keys = self.dm_keys().await;
        if peer_dm_pubkey == keys.dm_pubkey {
            return Ok(DmAck::Failed(DmNack::Rejected));
        }

        // Attribute the DM key to a user before any state is read or written. Without a
        // cached delegation the sender must run a handshake, which verifies its identity.
        let Some(peer_master_pubkey) = self
            .storage
            .get_master_pubkey_for_dm_pubkey(peer_dm_pubkey)
            .await
        else {
            return Ok(DmAck::Failed(DmNack::NoSession));
        };
        if self.storage.is_blocked(&peer_master_pubkey).await? {
            log::debug!(
                "[dm] rejecting envelope from blocked user {}",
                short_id(&peer_master_pubkey)
            );
            return Ok(DmAck::Failed(DmNack::Rejected));
        }

        // Held from loading the session until the advanced session is stored.
        let session_guard = tokio::time::timeout(
            SESSION_LOCK_TIMEOUT,
            self.session_locks.lock(peer_dm_pubkey),
        )
        .await
        .map_err(|_| AppError::Other("session is in use".into()))?;

        let Some(mut ratchet) = self.load_session(&keys, peer_dm_pubkey).await? else {
            return Ok(DmAck::Failed(DmNack::NoSession));
        };

        let plaintext = match ratchet.decrypt(
            &header,
            &envelope.ciphertext,
            &peer_x25519_public,
            &keys.x25519_public,
        ) {
            Ok(plaintext) => plaintext,
            // A local failure says nothing about the peer's session.
            Err(e @ (CryptoError::Rng | CryptoError::Internal)) => return Err(e.into()),
            Err(e) => {
                log::warn!(
                    "[dm] cannot decrypt envelope from {}: {e}",
                    short_id(&peer_master_pubkey)
                );
                return Ok(DmAck::Failed(DmNack::DecryptFailed));
            }
        };

        // The envelope authenticated, so the advanced session is stored in every branch
        // below, including for a payload that is rejected.
        let ratchet_sealed = seal_session(&keys.ratchet_storage_key, &ratchet)?;
        let now = now_millis();

        let payload = match serde_json::from_slice::<DmPayload>(&plaintext)
            .map_err(|e| e.to_string())
            .and_then(|payload| validate_payload(&payload).map(|()| payload))
        {
            Ok(payload) => payload,
            Err(e) => {
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, now)
                    .await?;
                log::warn!(
                    "[dm] rejecting payload from {}: {e}",
                    short_id(&peer_master_pubkey)
                );
                return Ok(DmAck::Failed(DmNack::Rejected));
            }
        };

        let conv_id = Storage::conversation_id(&keys.master_pubkey, &peer_master_pubkey);

        let received = match &payload {
            DmPayload::Message(msg) => {
                let stored_msg = StoredMessage {
                    id: msg.id.clone(),
                    conversation_id: conv_id.clone(),
                    from_pubkey: peer_master_pubkey.clone(),
                    to_pubkey: keys.master_pubkey.clone(),
                    content: msg.content.clone(),
                    // A timestamp after the local clock would keep the conversation at
                    // the top of the list.
                    timestamp: msg.timestamp.min(now),
                    media: msg.media.clone(),
                    read: false,
                    delivered: true,
                    reply_to: msg.reply_to.clone(),
                };
                let preview = crate::util::message_preview(&stored_msg.content);

                // Atomically save ratchet state + store message (single SQLite transaction)
                let inserted = self
                    .storage
                    .receive_dm_message_atomically(
                        peer_dm_pubkey,
                        &peer_master_pubkey,
                        &ratchet_sealed,
                        now,
                        &stored_msg,
                        &preview,
                    )
                    .await?;
                inserted.then_some(stored_msg)
            }
            _ => {
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, now)
                    .await?;
                None
            }
        };
        drop(session_guard);

        match payload {
            DmPayload::Message(msg) => match received {
                Some(stored_msg) => {
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
                None => log::debug!(
                    "[dm] message {} from {} is already stored",
                    short_id(&msg.id),
                    short_id(&peer_master_pubkey)
                ),
            },
            DmPayload::Delivered { message_id } => {
                // Only a message the local user sent in this conversation is updated.
                if self
                    .storage
                    .mark_dm_delivered(&conv_id, &keys.master_pubkey, &message_id)
                    .await?
                {
                    let _ = self.app_handle.emit(
                        "dm-delivered",
                        serde_json::json!({ "message_id": message_id }),
                    );
                }
            }
            DmPayload::Read { message_id } => {
                if self
                    .storage
                    .mark_dm_read_by_id(&conv_id, &keys.master_pubkey, &message_id)
                    .await?
                {
                    let _ = self
                        .app_handle
                        .emit("dm-read", serde_json::json!({ "message_id": message_id }));
                }
            }
            DmPayload::Typing => {
                let _ = self.app_handle.emit(
                    "typing-indicator",
                    serde_json::json!({ "peer": peer_master_pubkey }),
                );
            }
            payload @ (DmPayload::CallOffer { .. }
            | DmPayload::CallAnswer { .. }
            | DmPayload::CallReject { .. }
            | DmPayload::CallHangup { .. }) => {
                let _ = self.call_signal_tx.send(CallSignal {
                    peer_pubkey: peer_master_pubkey.clone(),
                    payload,
                });
            }
        }

        // The peer is reachable: attempt its queued messages without waiting for their
        // retry delay.
        self.retry_peer_outbox_now(&peer_master_pubkey).await;

        Ok(DmAck::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proscenium_types::DirectMessage;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn payload_ids_are_validated() {
        assert!(validate_payload(&DmPayload::Typing).is_ok());
        assert!(
            validate_payload(&DmPayload::Read {
                message_id: ID.to_string()
            })
            .is_ok()
        );
        assert!(
            validate_payload(&DmPayload::Read {
                message_id: "x".to_string()
            })
            .is_err()
        );
        assert!(
            validate_payload(&DmPayload::Delivered {
                message_id: ID.to_uppercase()
            })
            .is_err()
        );
        assert!(
            validate_payload(&DmPayload::CallOffer {
                call_id: ID.to_string(),
                video: false
            })
            .is_ok()
        );
        assert!(
            validate_payload(&DmPayload::CallHangup {
                call_id: String::new()
            })
            .is_err()
        );
        assert!(
            validate_payload(&DmPayload::Message(DirectMessage {
                id: "short".to_string(),
                content: String::new(),
                timestamp: 0,
                media: vec![],
                reply_to: None,
            }))
            .is_err()
        );
    }
}
