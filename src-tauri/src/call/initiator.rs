use crate::error::AppError;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{EndpointAddr, EndpointId};
use proscenium_types::{CALL_ALPN, CallState, DmPayload, is_valid_pubkey, short_id};

use super::state::{ActiveCall, CONNECT_TIMEOUT, CallPhase, IO_TIMEOUT, PeerNotice, RING_TIMEOUT};

impl super::CallHandler {
    /// Initiate an outgoing call to a peer: register the call, then send a CallOffer to
    /// the first device of the peer that answers. The audio connection is opened to the
    /// device that sends the answer.
    pub async fn start_call(&self, peer_pubkey: &str) -> Result<String, AppError> {
        if !is_valid_pubkey(peer_pubkey) {
            return Err(AppError::Other("invalid peer pubkey".into()));
        }
        let call_id = crate::util::generate_id();

        // Register the call before sending, so a second invocation fails while the
        // offer is in transit. The lock is released before the network send.
        let ring_timeout = {
            let mut lock = self.active_call.lock().await;
            if lock.is_some() {
                return Err(AppError::Other("already in a call".into()));
            }
            let call = ActiveCall::new(&call_id, peer_pubkey, None, true);
            let ring_timeout = call.phase_timeout.clone();
            *lock = Some(call);
            ring_timeout
        };

        log::info!(
            "[call] starting call {} to {}",
            short_id(&call_id),
            short_id(peer_pubkey)
        );

        let offer = DmPayload::CallOffer {
            call_id: call_id.clone(),
            video: false,
        };
        if let Err(e) = self.send_signal(peer_pubkey, None, offer).await {
            // The peer never saw the offer: remove the call without an event.
            let mut lock = self.active_call.lock().await;
            if lock.as_ref().is_some_and(|c| c.call_id == call_id)
                && let Some(call) = lock.take()
            {
                call.cancel.cancel();
            }
            return Err(e);
        }
        if ring_timeout.is_cancelled() {
            // The local user hung up while the offer was in transit.
            return Err(AppError::Other("call cancelled".into()));
        }

        self.emit_call_event(&call_id, peer_pubkey, CallState::Ringing);
        self.spawn_phase_timeout(
            &call_id,
            ring_timeout,
            RING_TIMEOUT,
            CallState::Failed,
            PeerNotice::Hangup,
        );

        Ok(call_id)
    }

    /// Called when the device `peer_device` of the peer sends a CallAnswer.
    /// Accepted only from the peer of the active outgoing call while it rings.
    /// Records `peer_device` as the call's device and starts a task that opens the audio
    /// connection to it.
    pub async fn on_call_answered(
        &self,
        call_id: &str,
        peer_pubkey: &str,
        peer_device: EndpointId,
    ) {
        let tokens = {
            let mut lock = self.active_call.lock().await;
            match lock.as_mut() {
                Some(call)
                    if call.call_id == call_id
                        && call.peer_pubkey == peer_pubkey
                        && call.outgoing
                        && call.phase == CallPhase::Ringing =>
                {
                    call.peer_device = Some(peer_device);
                    Some((call.enter_phase(CallPhase::Connecting), call.cancel.clone()))
                }
                _ => None,
            }
        };
        let Some((connect_timeout, call_cancel)) = tokens else {
            log::warn!(
                "[call] ignoring answer for call {} from {}",
                short_id(call_id),
                short_id(peer_pubkey)
            );
            return;
        };
        self.spawn_phase_timeout(
            call_id,
            connect_timeout,
            CONNECT_TIMEOUT,
            CallState::Failed,
            PeerNotice::Hangup,
        );
        self.emit_call_event(call_id, peer_pubkey, CallState::Active);

        // Connect in a separate task: the caller of this function processes all call
        // signals and must not wait for the connection.
        let handler = self.clone();
        let call_id = call_id.to_string();
        tokio::spawn(async move {
            let opened = tokio::select! {
                _ = call_cancel.cancelled() => return,
                opened = handler.open_audio(&call_id, peer_device) => opened,
            };
            match opened {
                Ok((conn, send, recv)) => {
                    handler.run_audio_session(conn, send, recv, &call_id).await;
                }
                Err(e) => {
                    log::error!("[call] audio connection failed: {e}");
                    handler
                        .end_call(&call_id, CallState::Failed, PeerNotice::Hangup)
                        .await;
                }
            }
        });
    }

    /// Caller side: dial the peer's device `target` on CALL_ALPN, which QUIC
    /// authenticates, open the audio stream and move the call to phase `Active`. Every
    /// step has a timeout. The callee sees the stream with the first audio frame and
    /// closes the connection if it does not serve it.
    async fn open_audio(
        &self,
        call_id: &str,
        target: EndpointId,
    ) -> Result<(Connection, SendStream, RecvStream), String> {
        let conn = tokio::time::timeout(
            CONNECT_TIMEOUT,
            self.endpoint.connect(EndpointAddr::from(target), CALL_ALPN),
        )
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| format!("connect failed: {e}"))?;

        let opened = async {
            let (send, recv) = tokio::time::timeout(IO_TIMEOUT, conn.open_bi())
                .await
                .map_err(|_| "opening the stream timed out".to_string())?
                .map_err(|e| format!("opening the stream failed: {e}"))?;

            let mut lock = self.active_call.lock().await;
            match lock.as_mut() {
                Some(call) if call.call_id == call_id && call.phase == CallPhase::Connecting => {
                    call.enter_phase(CallPhase::Active);
                    Ok((send, recv))
                }
                _ => Err("call ended while the connection opened".to_string()),
            }
        };
        match opened.await {
            Ok((send, recv)) => {
                log::info!(
                    "[call] audio connection established for {}",
                    short_id(call_id)
                );
                Ok((conn, send, recv))
            }
            Err(e) => {
                conn.close(1u32.into(), b"call setup failed");
                Err(e)
            }
        }
    }
}
