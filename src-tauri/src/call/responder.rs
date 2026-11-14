use crate::error::AppError;
use iroh::EndpointId;
use proscenium_types::{CallState, DmPayload, is_valid_id, short_id};

use super::state::{ActiveCall, CONNECT_TIMEOUT, CallPhase, PeerNotice, RING_TIMEOUT};

/// What `on_call_offer` does with an offer.
enum OfferDecision {
    /// Register the call and notify the user.
    Ring(tokio_util::sync::CancellationToken),
    /// Another call is active: reject the offer.
    Busy,
    /// The offer repeats the active call: ignore it.
    Duplicate,
}

impl super::CallHandler {
    /// Called when the device `peer_device` of a peer sends a CallOffer. The call's
    /// signals go to that device, and only that device may open its audio connection.
    ///
    /// An offer rings only if the peer is no stranger (`Storage::is_stranger`: followed,
    /// approved as follower, or written to in their conversation); any other offer is
    /// ignored without a reply, so generated identities cannot open the call overlay.
    pub async fn on_call_offer(&self, call_id: &str, peer_pubkey: &str, peer_device: EndpointId) {
        if !is_valid_id(call_id) {
            log::warn!(
                "[call] ignoring offer with an invalid call id from {}",
                short_id(peer_pubkey)
            );
            return;
        }
        let me = self.dm.identity.read().await.master_pubkey.clone();
        match self.storage.is_stranger(&me, peer_pubkey).await {
            Ok(false) => {}
            Ok(true) => {
                log::info!(
                    "[call] ignoring offer from {}: the caller is a stranger",
                    short_id(peer_pubkey)
                );
                return;
            }
            Err(e) => {
                log::error!("[call] ignoring offer: {e}");
                return;
            }
        }

        // The outcome is computed under the lock; nothing is sent while it is held.
        let decision = {
            let mut lock = self.active_call.lock().await;
            match lock.as_ref() {
                Some(call) if call.call_id == call_id => OfferDecision::Duplicate,
                Some(_) => OfferDecision::Busy,
                None => {
                    let call = ActiveCall::new(call_id, peer_pubkey, Some(peer_device), false);
                    let ring_timeout = call.phase_timeout.clone();
                    *lock = Some(call);
                    OfferDecision::Ring(ring_timeout)
                }
            }
        };

        match decision {
            OfferDecision::Duplicate => {}
            OfferDecision::Busy => {
                log::info!(
                    "[call] auto-rejecting offer {} (already in call)",
                    short_id(call_id)
                );
                // Sent by a separate task: the caller of this function processes all
                // call signals and must not wait for the delivery.
                let Ok(permit) = self.reject_permits.clone().try_acquire_owned() else {
                    return;
                };
                let handler = self.clone();
                let peer_pubkey = peer_pubkey.to_string();
                let reject = DmPayload::CallReject {
                    call_id: call_id.to_string(),
                };
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = handler
                        .send_signal(&peer_pubkey, Some(peer_device), reject)
                        .await;
                });
            }
            OfferDecision::Ring(ring_timeout) => {
                self.emit_call_event(call_id, peer_pubkey, CallState::Incoming);
                // Unanswered after 30 s: end the call and tell the caller.
                self.spawn_phase_timeout(
                    call_id,
                    ring_timeout,
                    RING_TIMEOUT,
                    CallState::Ended,
                    PeerNotice::Reject,
                );
            }
        }
    }

    /// Accept an incoming call. Sends CallAnswer via DM and waits for the
    /// caller to open the CALL_ALPN connection (handled by ProtocolHandler::accept).
    ///
    /// The call enters phase `Connecting` before the answer is sent, because the
    /// caller can dial before the delivery is confirmed. If the answer cannot be
    /// delivered and the caller's audio connection has not arrived, the call returns to
    /// phase `Incoming` with its ring timeout still running, and the error is returned. If
    /// the audio connection arrived (phase `Active`), the call is reported active.
    pub async fn accept_call(&self, call_id: &str) -> Result<(), AppError> {
        let (peer_pubkey, peer_device) = {
            let mut lock = self.active_call.lock().await;
            match lock.as_mut() {
                Some(call) if call.call_id == call_id && !call.outgoing => {
                    if call.phase != CallPhase::Incoming {
                        return Err(AppError::Other("call already accepted".into()));
                    }
                    call.phase = CallPhase::Connecting;
                    (call.peer_pubkey.clone(), call.peer_device)
                }
                _ => return Err(AppError::Other("no incoming call with that ID".into())),
            }
        };

        log::info!(
            "[call] accepting call {} from {}",
            short_id(call_id),
            short_id(&peer_pubkey)
        );

        let answer = DmPayload::CallAnswer {
            call_id: call_id.to_string(),
        };
        let sent = self.send_signal(&peer_pubkey, peer_device, answer).await;

        let mut lock = self.active_call.lock().await;
        let Some(call) = lock.as_mut().filter(|c| c.call_id == call_id) else {
            return Err(AppError::Other("call ended".into()));
        };
        // A caller that opened the audio connection received the answer, so a failed send
        // counts as delivered once the call is `Active`; the microphone is already on.
        if let Err(e) = sent
            && call.phase != CallPhase::Active
        {
            if call.phase == CallPhase::Connecting {
                call.phase = CallPhase::Incoming;
            }
            return Err(e);
        }
        // Replace the ring timeout with the timeout for the caller's audio connection,
        // unless that connection already arrived.
        let connect_timeout =
            (call.phase == CallPhase::Connecting).then(|| call.enter_phase(CallPhase::Connecting));
        drop(lock);

        if let Some(connect_timeout) = connect_timeout {
            self.spawn_phase_timeout(
                call_id,
                connect_timeout,
                CONNECT_TIMEOUT,
                CallState::Failed,
                PeerNotice::Hangup,
            );
        }
        self.emit_call_event(call_id, &peer_pubkey, CallState::Active);
        Ok(())
    }
}
