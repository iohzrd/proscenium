use crate::error::AppError;
use crate::storage::{ReceivedDm, Storage};
use iroh::EndpointId;
use proscenium_types::{
    DmAck, DmEnvelope, DmPayload, StoredMessage, is_valid_id, now_millis, short_id,
    verify_linked_devices_announcement,
};
use tauri::Emitter;

use super::{CallSignal, DmHandler, validate_direct_message};

/// Check a payload: the message limits of `validate_direct_message`, and the id format of
/// every message id and call id.
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

/// Reason to refuse `envelope` received from the device `remote` by `me`, or `None` if
/// it passes: the recipient is `me`, the sender is another user, and `sender_devices`
/// verifies and lists `remote`.
fn envelope_refusal(envelope: &DmEnvelope, remote: &str, me: &str) -> Option<&'static str> {
    if envelope.recipient != me {
        Some("the envelope is for another user")
    } else if envelope.sender_devices.master_pubkey == me {
        Some("the envelope is from the local identity")
    } else if verify_linked_devices_announcement(&envelope.sender_devices).is_err()
        || !envelope
            .sender_devices
            .devices
            .iter()
            .any(|device| device.node_id == remote)
    {
        Some("the device list does not list the sending device")
    } else {
        None
    }
}

impl DmHandler {
    /// Handle an envelope received from the device `remote` and return the answer.
    ///
    /// The envelope is applied only if `envelope_refusal` finds nothing, the sender is
    /// not blocked and the payload is valid. A message from a stranger beyond the limits
    /// of `Storage::receive_dm_message` is refused. The device list, delivered by `remote`,
    /// is cached under the rule of `Storage::cache_peer_announcement` after a message was
    /// stored, or for another payload if a conversation with the sender exists or the
    /// sender is followed, so signals from unknown identities add no rows.
    ///
    /// `Ok(DmAck::Rejected)` means the envelope was refused. `Err` means a local failure
    /// (storage); no answer is sent, so the sender keeps the message queued.
    pub(super) async fn handle_envelope(
        &self,
        remote: EndpointId,
        envelope: DmEnvelope,
    ) -> Result<DmAck, AppError> {
        let me = self.identity.read().await.master_pubkey.clone();
        let sender = envelope.sender_devices.master_pubkey.clone();
        if let Some(reason) = envelope_refusal(&envelope, &remote.to_string(), &me) {
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
        if let Err(reason) = validate_payload(&envelope.payload) {
            log::warn!(
                "[dm] rejecting envelope from {}: {reason}",
                short_id(&sender)
            );
            return Ok(DmAck::Rejected);
        }

        let known_sender = self.storage.has_conversation(&sender).await?
            || self.storage.is_following(&me, &sender).await?;
        let conv_id = Storage::conversation_id(&me, &sender);
        let mut cache_list = known_sender;
        match envelope.payload {
            DmPayload::Message(msg) => {
                if !known_sender
                    && self.storage.is_stranger(&me, &sender).await?
                    && !self.admit_new_stranger()
                {
                    log::warn!(
                        "[dm] refusing a new conversation from {}: limit of new conversations with unknown senders reached",
                        short_id(&sender)
                    );
                    return Ok(DmAck::Rejected);
                }
                let now = now_millis();
                let stored_msg = StoredMessage {
                    id: msg.id.clone(),
                    conversation_id: conv_id.clone(),
                    from_pubkey: sender.clone(),
                    to_pubkey: me.clone(),
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
                match self
                    .storage
                    .receive_dm_message(&sender, &stored_msg, &preview)
                    .await?
                {
                    ReceivedDm::Stored => {
                        cache_list = true;
                        log::info!("[dm] received message from {}", short_id(&sender));
                        let _ = self.app_handle.emit(
                            "dm-received",
                            serde_json::json!({
                                "from": sender,
                                "message": stored_msg,
                            }),
                        );
                    }
                    ReceivedDm::Duplicate => log::debug!(
                        "[dm] message {} from {} is already stored",
                        short_id(&msg.id),
                        short_id(&sender)
                    ),
                    ReceivedDm::Refused => {
                        log::warn!(
                            "[dm] refusing message from {}: message limit of an unanswered sender",
                            short_id(&sender)
                        );
                        return Ok(DmAck::Rejected);
                    }
                }
            }
            DmPayload::Delivered { message_id } => {
                // Only a message the local user sent in this conversation is updated.
                if self
                    .storage
                    .mark_dm_delivered(&conv_id, &me, &message_id)
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
                    .mark_dm_read_up_to(&conv_id, &me, &message_id)
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

        if cache_list
            && let Err(e) = self
                .storage
                .cache_peer_announcement(&envelope.sender_devices, Some(&remote.to_string()))
                .await
        {
            log::warn!(
                "[dm] failed to store the device list of {}: {e}",
                short_id(&sender)
            );
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
                removed: vec![],
                version: 1,
                signature: String::new(),
            };
            sign_linked_devices_announcement(&mut announcement, &self.master);
            announcement
        }

        fn envelope_to(&self, to: &Party) -> DmEnvelope {
            DmEnvelope {
                recipient: to.pubkey(),
                sender_devices: self.devices(),
                payload: DmPayload::Typing,
            }
        }
    }

    #[test]
    fn envelope_fields_are_checked() {
        let (alice, bob) = (Party::new(1), Party::new(2));
        let valid = alice.envelope_to(&bob);
        let check =
            |envelope: &DmEnvelope, remote: &str| envelope_refusal(envelope, remote, &bob.pubkey());
        assert_eq!(check(&valid, &alice.node_id()), None);

        // Sent by a device the sender's list does not list.
        assert!(check(&valid, &bob.node_id()).is_some());

        // Addressed to another user.
        let carol = Party::new(4);
        assert!(envelope_refusal(&valid, &alice.node_id(), &carol.pubkey()).is_some());

        // A list of another identity that does not list the sending device, and a changed
        // list.
        let mut foreign_list = valid.clone();
        foreign_list.sender_devices = carol.devices();
        assert!(check(&foreign_list, &alice.node_id()).is_some());
        let mut tampered_list = valid.clone();
        tampered_list.sender_devices.devices[0].node_id = carol.node_id();
        assert!(check(&tampered_list, &carol.node_id()).is_some());

        // From the local identity.
        let own = bob.envelope_to(&bob);
        assert!(envelope_refusal(&own, &bob.node_id(), &bob.pubkey()).is_some());
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
