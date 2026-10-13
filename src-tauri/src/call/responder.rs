use crate::error::AppError;
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
    /// Called when we receive a CallOffer from a peer (via DM handler).
    pub async fn on_call_offer(&self, call_id: &str, peer_pubkey: &str) {
        if !is_valid_id(call_id) {
            log::warn!(
                "[call] ignoring offer with an invalid call id from {}",
                short_id(peer_pubkey)
            );
            return;
        }

        // The outcome is computed under the lock; nothing is sent while it is held.
        let decision = {
            let mut lock = self.active_call.lock().await;
            match lock.as_ref() {
                Some(call) if call.call_id == call_id => OfferDecision::Duplicate,
                Some(_) => OfferDecision::Busy,
                None => {
                    let call = ActiveCall::new(call_id, peer_pubkey, false);
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
                    let _ = handler.send_signal(&peer_pubkey, reject).await;
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
    /// delivered the call returns to phase `Incoming` with its ring timeout still
    /// running, and the error is returned.
    pub async fn accept_call(&self, call_id: &str) -> Result<(), AppError> {
        let peer_pubkey = {
            let mut lock = self.active_call.lock().await;
            match lock.as_mut() {
                Some(call) if call.call_id == call_id && !call.outgoing => {
                    if call.phase != CallPhase::Incoming {
                        return Err(AppError::Other("call already accepted".into()));
                    }
                    call.phase = CallPhase::Connecting;
                    call.peer_pubkey.clone()
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
        let sent = self.send_signal(&peer_pubkey, answer).await;

        let mut lock = self.active_call.lock().await;
        let Some(call) = lock.as_mut().filter(|c| c.call_id == call_id) else {
            return Err(AppError::Other("call ended".into()));
        };
        if let Err(e) = sent {
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
