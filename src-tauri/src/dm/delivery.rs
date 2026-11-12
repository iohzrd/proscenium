use crate::constants::{
    DM_SEND_TIMEOUT, DM_SEND_WAIT, OUTBOX_FIRST_ATTEMPT_MARGIN, OUTBOX_FLUSH_INTERVAL,
    OUTBOX_MAX_AGE, OUTBOX_PASS_LIMIT, OUTBOX_RETRY_BASE, OUTBOX_RETRY_MAX,
};
use crate::error::AppError;
use crate::storage::Storage;
use iroh::EndpointId;
use proscenium_types::{
    DirectMessage, DmAck, DmEnvelope, DmPayload, StoredMessage, now_millis, short_id,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::Emitter;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::{ACK_TIMEOUT, DmHandler, MAX_DM_PAYLOAD_LEN};

/// Answer of a peer that ends the delivery of a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// A device answered `DmAck::Ok`.
    Acknowledged,
    /// No device answered `DmAck::Ok` and one answered `DmAck::Rejected`. Sending again
    /// gives the same answer.
    Rejected,
}

/// The error of a device that answered `DmAck::Deferred`.
fn deferred() -> AppError {
    AppError::Other(
        "the recipient deferred the message: its limit of new conversations was reached".into(),
    )
}

/// Devices of the recipient a payload is sent to.
#[derive(Debug, Clone, Copy)]
enum Target {
    /// Every device of the cached device list, at the same time.
    AllDevices,
    /// The devices of the cached device list in order, until one answers `DmAck::Ok`.
    FirstDevice,
    /// One device.
    Device(EndpointId),
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
    /// The stored payload did not parse; the entry was removed.
    Invalid,
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

    /// Send a typing signal or read receipt to every device of `peer_pubkey` without
    /// storing or queueing it. Returns an error unless a device answered `DmAck::Ok`.
    pub async fn send_signal(&self, peer_pubkey: &str, payload: DmPayload) -> Result<(), AppError> {
        self.send_unqueued(peer_pubkey, Target::AllDevices, &payload)
            .await
    }

    /// Send a call signal to `device` of `peer_pubkey`, or with `device` `None` to the
    /// first device of its device list that answers. Returns an error unless that device
    /// answered `DmAck::Ok`.
    pub async fn send_call_signal(
        &self,
        peer_pubkey: &str,
        device: Option<EndpointId>,
        payload: DmPayload,
    ) -> Result<(), AppError> {
        let target = device.map_or(Target::FirstDevice, Target::Device);
        self.send_unqueued(peer_pubkey, target, &payload).await
    }

    async fn send_unqueued(
        &self,
        peer_pubkey: &str,
        target: Target,
        payload: &DmPayload,
    ) -> Result<(), AppError> {
        match self.deliver(peer_pubkey, target, payload).await? {
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

        let payload = match serde_json::from_str::<DmPayload>(&payload_json) {
            Ok(payload) => payload,
            Err(e) => {
                log::error!(
                    "[dm] outbox entry {} does not parse, removing it: {e}",
                    short_id(message_id)
                );
                if let Err(e) = self.storage.remove_outbox_message(message_id).await {
                    log::error!(
                        "[dm] failed to remove outbox entry {}: {e}",
                        short_id(message_id)
                    );
                }
                return QueuedOutcome::Invalid;
            }
        };

        match self
            .deliver(peer_pubkey, Target::AllDevices, &payload)
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

    /// Send `payload` in a `DmEnvelope` to each device `target` names, within
    /// `DM_SEND_TIMEOUT` in total. The envelope carries the local signed device list.
    ///
    /// Returns `Acknowledged` as soon as a device answers `DmAck::Ok`; the sends to the
    /// other devices continue in the background. `Target::FirstDevice` tries the devices
    /// in order until one answers `DmAck::Ok`. Returns `Rejected` only if every device
    /// answered `DmAck::Rejected`; otherwise the error of a device that did not answer or
    /// answered `DmAck::Deferred`, so a queued payload is sent again later.
    async fn deliver(
        &self,
        peer_master_pubkey: &str,
        target: Target,
        payload: &DmPayload,
    ) -> Result<Delivery, AppError> {
        tokio::time::timeout(DM_SEND_TIMEOUT, async {
            let envelope = DmEnvelope {
                recipient: peer_master_pubkey.to_string(),
                sender_devices: crate::announcement::build_own_announcement(
                    &self.storage,
                    &self.identity,
                )
                .await?,
                payload: payload.clone(),
            };
            let frame: Arc<[u8]> = serde_json::to_vec(&envelope)?.into();
            let devices = match target {
                Target::Device(device) => vec![device],
                Target::AllDevices | Target::FirstDevice => {
                    self.peer_devices(peer_master_pubkey).await?
                }
            };

            let mut rejected = false;
            let mut last_error = None;
            match target {
                Target::FirstDevice => {
                    for device in &devices {
                        match self.send_frame(*device, &frame).await {
                            Ok(DmAck::Ok) => return Ok(Delivery::Acknowledged),
                            Ok(DmAck::Rejected) => rejected = true,
                            Ok(DmAck::Deferred) => last_error = Some(deferred()),
                            Err(e) => last_error = Some(e),
                        }
                    }
                }
                Target::AllDevices | Target::Device(_) => {
                    let mut sends = JoinSet::new();
                    for device in &devices {
                        let handler = self.clone();
                        let (device, frame) = (*device, frame.clone());
                        sends.spawn(async move { handler.send_frame(device, &frame).await });
                    }
                    while let Some(sent) = sends.join_next().await {
                        match sent {
                            Ok(Ok(DmAck::Ok)) => {
                                // The other devices receive their envelopes in the
                                // background; each send has its own timeouts.
                                sends.detach_all();
                                return Ok(Delivery::Acknowledged);
                            }
                            Ok(Ok(DmAck::Rejected)) => rejected = true,
                            Ok(Ok(DmAck::Deferred)) => last_error = Some(deferred()),
                            Ok(Err(e)) => last_error = Some(e),
                            Err(e) => last_error = Some(AppError::Other(e.to_string())),
                        }
                    }
                }
            }
            // A device that did not answer can still accept the envelope later, so the
            // envelope counts as rejected only if every device answered `Rejected`.
            match last_error {
                Some(e) => Err(e),
                None if rejected => Ok(Delivery::Rejected),
                None => Err(AppError::Other(format!(
                    "no device of {} answered",
                    short_id(peer_master_pubkey)
                ))),
            }
        })
        .await
        .map_err(|_| AppError::Other("send timed out".into()))?
    }

    /// Send one envelope `frame` to `device` and return its answer.
    async fn send_frame(&self, device: EndpointId, frame: &[u8]) -> Result<DmAck, AppError> {
        let answer = self.exchange(device, frame, ACK_TIMEOUT).await;
        let ack = answer.and_then(|answer| Ok(serde_json::from_slice::<DmAck>(&answer)?));
        if let Err(e) = &ack {
            log::debug!(
                "[dm] no acknowledgement from node {}: {e}",
                short_id(&device.to_string())
            );
        }
        ack
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
