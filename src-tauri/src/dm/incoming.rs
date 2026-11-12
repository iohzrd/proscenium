use crate::error::AppError;
use crate::storage::Storage;
use iroh::EndpointId;
use proscenium_types::{
    DmAck, DmEnvelope, DmPayload, StoredMessage, is_valid_id, now_millis, short_id,
    verify_linked_devices_announcement,
};
use tauri::Emitter;

use super::{CallSignal, DmHandler, MAX_DM_CIPHERTEXT_LEN, envelope, validate_direct_message};

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

/// Reason to refuse `envelope` received from the device `remote` by the device
/// `my_device` of `me`, or `None` if its fields pass: `remote` is `sender_device`, the
/// recipient fields name `me` and `my_device`, the sender is another user, the ciphertext
/// fits, and `sender_devices` verifies, belongs to `sender` and lists `sender_device`.
fn envelope_refusal(
    envelope: &DmEnvelope,
    remote: &str,
    me: &str,
    my_device: &str,
) -> Option<&'static str> {
    if envelope.sender_device != remote {
        Some("the envelope was not sent by its sender device")
    } else if envelope.recipient != me || envelope.recipient_device != my_device {
        Some("the envelope is for another device")
    } else if envelope.sender == me {
        Some("the envelope is from the local identity")
    } else if envelope.ciphertext.len() > MAX_DM_CIPHERTEXT_LEN {
        Some("the ciphertext is too long")
    } else if envelope.sender_devices.master_pubkey != envelope.sender
        || verify_linked_devices_announcement(&envelope.sender_devices).is_err()
        || !envelope
            .sender_devices
            .devices
            .iter()
            .any(|device| device.node_id == envelope.sender_device)
    {
        Some("the device list does not list the sender device")
    } else {
        None
    }
}

impl DmHandler {
    /// Handle an envelope received from the device `remote` and return the answer.
    ///
    /// The envelope is applied only if `envelope_refusal` finds nothing, the sender is
    /// not blocked, the ciphertext decrypts and the payload is valid. Then the device list
    /// is cached, delivered by `remote`, under the rule of
    /// `Storage::cache_peer_announcement`.
    ///
    /// `Ok(DmAck::Rejected)` means the envelope was refused. `Err` means a local failure
    /// (storage); no answer is sent, so the sender keeps the message queued.
    pub(super) async fn handle_envelope(
        &self,
        remote: EndpointId,
        envelope: DmEnvelope,
    ) -> Result<DmAck, AppError> {
        let keys = self.dm_keys().await;
        let sender = envelope.sender.clone();
        let my_device = keys.device_key.public().to_string();
        if let Some(reason) = envelope_refusal(
            &envelope,
            &remote.to_string(),
            &keys.master_pubkey,
            &my_device,
        ) {
            log::warn!(
                "[dm] rejecting envelope from node {}: {reason}",
                short_id(&remote.to_string())
            );
            return Ok(DmAck::Rejected);
        }
        if self.storage.is_blocked(&sender).await? {
            log::debug!(
                "[dm] rejecting envelope from blocked user {}",
                short_id(&sender)
            );
            return Ok(DmAck::Rejected);
        }
        let payload = match envelope::open(&keys.device_key, &envelope).and_then(|plaintext| {
            let payload = serde_json::from_slice::<DmPayload>(&plaintext)?;
            validate_payload(&payload).map_err(AppError::Other)?;
            Ok(payload)
        }) {
            Ok(payload) => payload,
            Err(e) => {
                log::warn!("[dm] rejecting envelope from {}: {e}", short_id(&sender));
                return Ok(DmAck::Rejected);
            }
        };

        if let Err(e) = self
            .storage
            .cache_peer_announcement(&envelope.sender_devices, Some(&remote.to_string()))
            .await
        {
            log::warn!(
                "[dm] failed to store the device list of {}: {e}",
                short_id(&sender)
            );
        }

        let conv_id = Storage::conversation_id(&keys.master_pubkey, &sender);
        match payload {
            DmPayload::Message(msg) => {
                let now = now_millis();
                let stored_msg = StoredMessage {
                    id: msg.id.clone(),
                    conversation_id: conv_id.clone(),
                    from_pubkey: sender.clone(),
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
                if self
                    .storage
                    .receive_dm_message(&sender, &stored_msg, &preview)
                    .await?
                {
                    log::info!("[dm] received message from {}", short_id(&sender));
                    let _ = self.app_handle.emit(
                        "dm-received",
                        serde_json::json!({
                            "from": sender,
                            "message": stored_msg,
                        }),
                    );
                } else {
                    log::debug!(
                        "[dm] message {} from {} is already stored",
                        short_id(&msg.id),
                        short_id(&sender)
                    );
                }
            }
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
                let _ = self
                    .app_handle
                    .emit("typing-indicator", serde_json::json!({ "peer": sender }));
            }
            payload @ (DmPayload::CallOffer { .. }
            | DmPayload::CallAnswer { .. }
            | DmPayload::CallReject { .. }
            | DmPayload::CallHangup { .. }) => {
                let _ = self.call_signal_tx.send(CallSignal {
                    peer_pubkey: sender.clone(),
                    peer_device: remote,
                    payload,
                });
            }
        }

        // The peer is reachable: attempt its queued messages without waiting for their
        // retry delay.
        self.retry_peer_outbox_now(&sender).await;

        Ok(DmAck::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use proscenium_types::{
        DeviceEntry, DirectMessage, LinkedDevicesAnnouncement, sign_linked_devices_announcement,
    };

    struct Party {
        master: SecretKey,
        device: SecretKey,
    }

    impl Party {
        fn new(seed: u8) -> Self {
            Self {
                master: SecretKey::from_bytes(&[seed; 32]),
                device: SecretKey::from_bytes(&[seed.wrapping_add(100); 32]),
            }
        }

        fn pubkey(&self) -> String {
            self.master.public().to_string()
        }

        fn node_id(&self) -> String {
            self.device.public().to_string()
        }

        fn devices(&self) -> LinkedDevicesAnnouncement {
            let mut announcement = LinkedDevicesAnnouncement {
                master_pubkey: self.pubkey(),
                devices: vec![DeviceEntry {
                    node_id: self.node_id(),
                    device_name: "d".to_string(),
                    added_at: 1,
                }],
                version: 1,
                signature: String::new(),
            };
            sign_linked_devices_announcement(&mut announcement, &self.master);
            announcement
        }

        fn seal_to(&self, to: &Party, plaintext: &[u8]) -> DmEnvelope {
            envelope::seal(
                &self.device,
                &self.pubkey(),
                &self.devices(),
                &to.pubkey(),
                &to.device.public(),
                plaintext,
            )
            .unwrap()
        }
    }

    #[test]
    fn envelope_fields_are_checked_before_decryption() {
        let (alice, bob) = (Party::new(1), Party::new(2));
        let valid = alice.seal_to(&bob, b"{}");
        let check = |envelope: &DmEnvelope, remote: &str| {
            envelope_refusal(envelope, remote, &bob.pubkey(), &bob.node_id())
        };
        assert_eq!(check(&valid, &alice.node_id()), None);
        assert_eq!(
            envelope::open(&bob.device, &valid).unwrap().as_slice(),
            b"{}"
        );

        // Delivered by another node than the sender device.
        assert!(check(&valid, &bob.node_id()).is_some());

        // Addressed to another device of the recipient.
        let other_device = Party::new(3).node_id();
        assert!(envelope_refusal(&valid, &alice.node_id(), &bob.pubkey(), &other_device).is_some());

        // A device list of another identity, or one that does not list the sender device.
        let carol = Party::new(4);
        let mut foreign_list = valid.clone();
        foreign_list.sender_devices = carol.devices();
        assert!(check(&foreign_list, &alice.node_id()).is_some());
        let mut tampered_list = valid.clone();
        tampered_list.sender_devices.devices[0].node_id = carol.node_id();
        assert!(check(&tampered_list, &alice.node_id()).is_some());

        // From the local identity.
        let own = bob.seal_to(&bob, b"{}");
        assert!(envelope_refusal(&own, &bob.node_id(), &bob.pubkey(), &bob.node_id()).is_some());

        // Oversized ciphertext.
        let mut long = valid;
        long.ciphertext = vec![0; MAX_DM_CIPHERTEXT_LEN + 1];
        assert!(check(&long, &alice.node_id()).is_some());
    }

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
