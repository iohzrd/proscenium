use crate::constants::{
    DM_SEND_TIMEOUT, DM_SEND_WAIT, OUTBOX_FIRST_ATTEMPT_MARGIN, OUTBOX_FLUSH_INTERVAL,
    OUTBOX_MAX_AGE, OUTBOX_PASS_LIMIT, OUTBOX_RETRY_BASE, OUTBOX_RETRY_MAX,
};
use crate::error::AppError;
use crate::storage::Storage;
use proscenium_types::{
    DirectMessage, DmAck, DmMessage, DmNack, DmPayload, EncryptedEnvelope, StoredMessage,
    now_millis, short_id,
};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::Emitter;
use tokio_util::sync::CancellationToken;

use super::{ACK_TIMEOUT, DmHandler, MAX_DM_PAYLOAD_LEN, PeerRoute};

/// Answer of a peer that ends the delivery of a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// The peer answered `DmAck::Ok`.
    Acknowledged,
    /// The peer answered `DmNack::Rejected`. Sending again gives the same answer.
    Rejected,
}

/// Result of one delivery attempt of an outbox entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuedOutcome {
    /// Acknowledged; the entry was removed and the message marked delivered.
    Delivered,
    /// Rejected by the peer; the entry was removed.
    Rejected,
    /// Not acknowledged; the entry stays queued with a later attempt time.
    Failed,
    /// The entry no longer exists (delivered or deleted in the meantime).
    Gone,
}

/// Delay before the next delivery attempt after `attempts` failed attempts:
/// `OUTBOX_RETRY_BASE * 2^(attempts - 1)`, at most `OUTBOX_RETRY_MAX`.
fn outbox_retry_delay(attempts: u32) -> Duration {
    let exponent = attempts.saturating_sub(1).min(16);
    OUTBOX_RETRY_BASE
        .saturating_mul(1u32 << exponent)
        .min(OUTBOX_RETRY_MAX)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl DmHandler {
    /// Spawn the outbox flush loop. A pass runs when `outbox_notify` is notified and at
    /// least every `OUTBOX_FLUSH_INTERVAL`. Cancelling `token` stops the loop, also
    /// during a pass.
    pub fn start_background(&self, token: CancellationToken) {
        let handler = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = handler.outbox_notify.notified() => {}
                    _ = tokio::time::sleep(OUTBOX_FLUSH_INTERVAL) => {}
                }
                let ignore_delay = handler.outbox_ignore_delay.swap(false, Ordering::Relaxed);
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = handler.flush_outbox(ignore_delay) => {}
                }
            }
            log::info!("[dm] outbox flush loop stopped");
        });
    }

    /// Wake the flush loop for a pass that also attempts entries whose retry delay has
    /// not elapsed.
    pub fn request_outbox_flush(&self) {
        self.outbox_ignore_delay.store(true, Ordering::Relaxed);
        self.outbox_notify.notify_one();
    }

    /// One flush pass: discard entries older than `OUTBOX_MAX_AGE`, then attempt the due
    /// entries oldest first. After a failed attempt the remaining entries for that peer
    /// are skipped in this pass.
    async fn flush_outbox(&self, ignore_delay: bool) {
        let now = now_millis();
        match self
            .storage
            .delete_expired_outbox(now.saturating_sub(millis(OUTBOX_MAX_AGE)))
            .await
        {
            Ok(0) => {}
            Ok(discarded) => log::warn!(
                "[dm] discarded {discarded} undelivered messages queued for more than {} days",
                OUTBOX_MAX_AGE.as_secs() / 86_400
            ),
            Err(e) => log::error!("[dm] failed to discard expired outbox entries: {e}"),
        }

        let entries = match self
            .storage
            .get_due_outbox_entries(now, ignore_delay, OUTBOX_PASS_LIMIT)
            .await
        {
            Ok(entries) => entries,
            Err(e) => {
                log::error!("[dm] failed to read outbox: {e}");
                return;
            }
        };

        let mut failed_peers: HashSet<String> = HashSet::new();
        for (message_id, peer_pubkey, attempts) in entries {
            if failed_peers.contains(&peer_pubkey) {
                continue;
            }
            if self
                .deliver_queued(&message_id, &peer_pubkey, attempts)
                .await
                == QueuedOutcome::Failed
            {
                failed_peers.insert(peer_pubkey);
            }
        }
    }

    /// Store a message written by the local user together with its outbox entry, then
    /// start its first delivery attempt and wait up to `DM_SEND_WAIT` for it.
    ///
    /// Returns true if the peer acknowledged the message within that time. Otherwise the
    /// attempt continues in a background task; an undelivered message stays in the outbox
    /// and `dm-delivered` is emitted when a later attempt succeeds.
    /// Returns an error, and stores nothing, if the message exceeds a size limit.
    pub async fn send_dm(&self, message: &StoredMessage) -> Result<bool, AppError> {
        let direct_message = DirectMessage {
            id: message.id.clone(),
            content: message.content.clone(),
            timestamp: message.timestamp,
            media: message.media.clone(),
            reply_to: message.reply_to.clone(),
        };
        super::validate_direct_message(&direct_message)?;
        let payload_json = serde_json::to_string(&DmPayload::Message(direct_message))?;
        if payload_json.len() > MAX_DM_PAYLOAD_LEN {
            return Err(AppError::Other(format!(
                "message too large: {} bytes serialized (max {MAX_DM_PAYLOAD_LEN})",
                payload_json.len()
            )));
        }

        // The flush loop must not select the entry while the first attempt is running.
        let first_flush_at = now_millis()
            .saturating_add(millis(DM_SEND_TIMEOUT))
            .saturating_add(millis(OUTBOX_FIRST_ATTEMPT_MARGIN));
        let preview = crate::util::message_preview(&message.content);
        self.storage
            .insert_outgoing_dm(message, &preview, &payload_json, first_flush_at)
            .await?;

        let handler = self.clone();
        let message_id = message.id.clone();
        let peer_pubkey = message.to_pubkey.clone();
        let first_attempt =
            tokio::spawn(async move { handler.deliver_queued(&message_id, &peer_pubkey, 0).await });
        Ok(matches!(
            tokio::time::timeout(DM_SEND_WAIT, first_attempt).await,
            Ok(Ok(QueuedOutcome::Delivered))
        ))
    }

    /// Send a DM signal (typing, read receipt, call signal) without storing or queueing it.
    /// Returns an error unless the peer answered `DmAck::Ok`.
    /// A typing signal does not start a handshake.
    pub async fn send_signal(&self, peer_pubkey: &str, payload: DmPayload) -> Result<(), AppError> {
        let allow_handshake = !matches!(payload, DmPayload::Typing);
        let plaintext = serde_json::to_vec(&payload)?;
        match self
            .deliver(peer_pubkey, &plaintext, allow_handshake)
            .await?
        {
            Delivery::Acknowledged => Ok(()),
            Delivery::Rejected => Err(AppError::Other(format!(
                "{} rejected the signal",
                short_id(peer_pubkey)
            ))),
        }
    }

    /// Attempt delivery of one outbox entry and record the result in storage.
    /// `attempts` is the entry's number of failed attempts so far.
    async fn deliver_queued(
        &self,
        message_id: &str,
        peer_pubkey: &str,
        attempts: u32,
    ) -> QueuedOutcome {
        // Read the payload at send time: the entry may have been delivered or deleted
        // since it was selected.
        let payload_json = match self.storage.get_outbox_payload(message_id).await {
            Ok(Some(payload_json)) => payload_json,
            Ok(None) => return QueuedOutcome::Gone,
            Err(e) => {
                log::error!(
                    "[dm] failed to read outbox entry {}: {e}",
                    short_id(message_id)
                );
                return QueuedOutcome::Failed;
            }
        };

        match self
            .deliver(peer_pubkey, payload_json.as_bytes(), true)
            .await
        {
            Ok(Delivery::Acknowledged) => {
                log::info!("[dm] delivered message to {}", short_id(peer_pubkey));
                self.mark_delivered(peer_pubkey, message_id).await;
                self.retry_peer_outbox_now(peer_pubkey).await;
                QueuedOutcome::Delivered
            }
            Ok(Delivery::Rejected) => {
                log::warn!(
                    "[dm] {} rejected message {}; it is not sent again",
                    short_id(peer_pubkey),
                    short_id(message_id)
                );
                if let Err(e) = self.storage.remove_outbox_message(message_id).await {
                    log::error!(
                        "[dm] failed to remove outbox entry {}: {e}",
                        short_id(message_id)
                    );
                }
                QueuedOutcome::Rejected
            }
            Err(e) => {
                let delay = outbox_retry_delay(attempts.saturating_add(1));
                log::info!(
                    "[dm] message {} to {} not delivered (attempt {}), next attempt in {} s: {e}",
                    short_id(message_id),
                    short_id(peer_pubkey),
                    attempts.saturating_add(1),
                    delay.as_secs()
                );
                let next_attempt_at = now_millis().saturating_add(millis(delay));
                if let Err(e) = self
                    .storage
                    .record_outbox_failure(message_id, peer_pubkey, next_attempt_at)
                    .await
                {
                    log::error!(
                        "[dm] failed to record outbox failure for {}: {e}",
                        short_id(message_id)
                    );
                }
                QueuedOutcome::Failed
            }
        }
    }

    /// Mark a message the local user sent to `peer_pubkey` as delivered, remove its outbox
    /// entry and emit `dm-delivered`.
    async fn mark_delivered(&self, peer_pubkey: &str, message_id: &str) {
        let my_pubkey = self.identity.read().await.master_pubkey.clone();
        let conv_id = Storage::conversation_id(&my_pubkey, peer_pubkey);
        match self
            .storage
            .mark_dm_delivered(&conv_id, &my_pubkey, message_id)
            .await
        {
            Ok(true) => {
                let _ = self.app_handle.emit(
                    "dm-delivered",
                    serde_json::json!({ "message_id": message_id }),
                );
            }
            // The message was deleted while it was being sent; its outbox entry went
            // with it.
            Ok(false) => {}
            Err(e) => log::error!(
                "[dm] failed to mark delivered {}: {e}",
                short_id(message_id)
            ),
        }
    }

    /// Clear the retry delay of the entries queued for `peer_pubkey` and wake the flush
    /// loop if any entry changed. Called when the peer answered or sent an envelope.
    pub(super) async fn retry_peer_outbox_now(&self, peer_pubkey: &str) {
        match self
            .storage
            .make_peer_outbox_due(peer_pubkey, now_millis())
            .await
        {
            Ok(0) => {}
            Ok(_) => self.outbox_notify.notify_one(),
            Err(e) => log::error!(
                "[dm] failed to update outbox entries for {}: {e}",
                short_id(peer_pubkey)
            ),
        }
    }

    /// Encrypt `plaintext` (a serialized `DmPayload`) for `peer_master_pubkey` and send it,
    /// within `DM_SEND_TIMEOUT` in total.
    ///
    /// If the peer answers `NoSession` or `DecryptFailed`, the session that produced the
    /// envelope is deleted, a new handshake runs, and the payload is encrypted and sent
    /// once more. Returns an error if no `DmAck::Ok` or `DmNack::Rejected` was received.
    async fn deliver(
        &self,
        peer_master_pubkey: &str,
        plaintext: &[u8],
        allow_handshake: bool,
    ) -> Result<Delivery, AppError> {
        tokio::time::timeout(DM_SEND_TIMEOUT, async {
            let keys = self.dm_keys().await;
            let mut peer = self.resolve_peer(peer_master_pubkey).await?;
            // Ratchet key of the envelope the peer could not decrypt, once that happened.
            let mut refused_ratchet_key: Option<String> = None;
            loop {
                let envelope = self
                    .encrypt_for_peer(
                        &keys,
                        &mut peer,
                        plaintext,
                        refused_ratchet_key.as_deref(),
                        allow_handshake,
                    )
                    .await?;
                let ratchet_key = envelope.ratchet_header.dh_public.clone();
                match self.send_envelope(&peer, envelope).await? {
                    DmAck::Ok => return Ok(Delivery::Acknowledged),
                    DmAck::Failed(DmNack::Rejected) => return Ok(Delivery::Rejected),
                    DmAck::Failed(nack) if allow_handshake && refused_ratchet_key.is_none() => {
                        log::info!(
                            "[dm] {} answered {nack:?}; encrypting again with a new session",
                            short_id(peer_master_pubkey)
                        );
                        refused_ratchet_key = Some(ratchet_key);
                    }
                    DmAck::Failed(nack) => {
                        return Err(AppError::Other(format!("peer answered {nack:?}")));
                    }
                }
            }
        })
        .await
        .map_err(|_| AppError::Other("send timed out".into()))?
    }

    /// Send an envelope to the NodeIds of `peer`, in order, until one answers with a
    /// `DmAck`. Returns that answer, or the last error if no NodeId answered.
    async fn send_envelope(
        &self,
        peer: &PeerRoute,
        envelope: EncryptedEnvelope,
    ) -> Result<DmAck, AppError> {
        let frame = serde_json::to_vec(&DmMessage::Envelope(envelope))?;
        let mut last_error = AppError::Other(format!(
            "no transport NodeId for {}",
            short_id(&peer.master_pubkey)
        ));
        for node in &peer.node_ids {
            let answer = match self.exchange(*node, &frame, ACK_TIMEOUT).await {
                Ok(answer) => serde_json::from_slice::<DmAck>(&answer).map_err(AppError::from),
                Err(e) => Err(e),
            };
            match answer {
                Ok(ack) => return Ok(ack),
                Err(e) => {
                    log::debug!(
                        "[dm] no acknowledgement from {} at node {}: {e}",
                        short_id(&peer.master_pubkey),
                        short_id(&node.to_string())
                    );
                    last_error = e;
                }
            }
        }
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_doubles_up_to_the_maximum() {
        assert_eq!(outbox_retry_delay(0), OUTBOX_RETRY_BASE);
        assert_eq!(outbox_retry_delay(1), OUTBOX_RETRY_BASE);
        assert_eq!(outbox_retry_delay(2), OUTBOX_RETRY_BASE * 2);
        assert_eq!(outbox_retry_delay(3), OUTBOX_RETRY_BASE * 4);
        assert_eq!(outbox_retry_delay(40), OUTBOX_RETRY_MAX);
        assert_eq!(outbox_retry_delay(u32::MAX), OUTBOX_RETRY_MAX);
        let mut previous = Duration::ZERO;
        for attempts in 1..64 {
            let delay = outbox_retry_delay(attempts);
            assert!(delay >= previous && delay <= OUTBOX_RETRY_MAX);
            previous = delay;
        }
    }
}
