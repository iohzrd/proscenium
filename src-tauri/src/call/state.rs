use crate::audio::{SharedCapture, SharedPlayback, android, lock};
use crate::error::AppError;
use crate::preferences;
use iroh::EndpointId;
use proscenium_types::{CallState, DmPayload, short_id};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Time an offer may stay unanswered.
pub(super) const RING_TIMEOUT: Duration = Duration::from_secs(30);
/// Time the audio connection may take after the call was answered.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for one step of the audio handshake and for one audio frame write.
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Total time for delivering one call signal over the DM channel.
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(20);

/// Phase of the active call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CallPhase {
    /// Outgoing call: the offer was sent (or is being sent) and no answer arrived.
    Ringing,
    /// Incoming call: the offer arrived and the local user has not answered.
    Incoming,
    /// The call was answered and the audio connection is not established.
    /// Outgoing call: this node dials. Incoming call: this node waits for the caller.
    Connecting,
    /// The audio session runs. Microphone capture starts only in this phase.
    Active,
}

/// How the peer learns that the call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PeerNotice {
    /// The peer ended the call or was already told.
    None,
    Hangup,
    Reject,
}

/// Active call state tracked by the handler.
pub(crate) struct ActiveCall {
    pub(super) call_id: String,
    pub(super) peer_pubkey: String,
    /// The peer's device in this call: for an incoming call the device that sent the
    /// offer, for an outgoing call the device that answered. Call signals go to this
    /// device, and the audio connection runs between it and this device.
    pub(super) peer_device: Option<EndpointId>,
    /// True if the local user placed the call.
    pub(super) outgoing: bool,
    pub(super) phase: CallPhase,
    /// Cancelled when the call ends. Parent of `phase_timeout` and of the audio
    /// session token.
    pub(super) cancel: CancellationToken,
    /// Cancels the timeout task of the current phase.
    pub(super) phase_timeout: CancellationToken,
    pub(super) muted: Arc<AtomicBool>,
    /// Shared handle to the active audio capture (if audio session is running).
    pub(super) capture: SharedCapture,
    pub(super) playback: SharedPlayback,
}

impl ActiveCall {
    pub(super) fn new(
        call_id: &str,
        peer_pubkey: &str,
        peer_device: Option<EndpointId>,
        outgoing: bool,
    ) -> Self {
        let cancel = CancellationToken::new();
        let phase_timeout = cancel.child_token();
        Self {
            call_id: call_id.to_string(),
            peer_pubkey: peer_pubkey.to_string(),
            peer_device,
            outgoing,
            phase: if outgoing {
                CallPhase::Ringing
            } else {
                CallPhase::Incoming
            },
            cancel,
            phase_timeout,
            muted: Arc::new(AtomicBool::new(false)),
            capture: Arc::new(std::sync::Mutex::new(None)),
            playback: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Enter `phase`: cancel the timeout of the previous phase and return the token for
    /// the timeout of the new one.
    pub(super) fn enter_phase(&mut self, phase: CallPhase) -> CancellationToken {
        self.phase = phase;
        self.phase_timeout.cancel();
        self.phase_timeout = self.cancel.child_token();
        self.phase_timeout.clone()
    }
}

impl super::CallHandler {
    /// Load audio device preferences from storage.
    /// Returns (input_device, output_device) where None means "use default".
    pub(super) async fn audio_device_prefs(&self) -> (Option<String>, Option<String>) {
        let input = self
            .storage
            .get_preference(preferences::AUDIO_INPUT_DEVICE)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        let output = self
            .storage
            .get_preference(preferences::AUDIO_OUTPUT_DEVICE)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        (input, output)
    }

    /// Send a call signal over the DM channel with a total timeout: to `device` of
    /// `peer_pubkey`, or with `device` `None` to the first device of its device list that
    /// answers. The `active_call` lock must not be held by the caller.
    pub(super) async fn send_signal(
        &self,
        peer_pubkey: &str,
        device: Option<EndpointId>,
        payload: DmPayload,
    ) -> Result<(), AppError> {
        tokio::time::timeout(
            SIGNAL_TIMEOUT,
            self.dm.send_call_signal(peer_pubkey, device, payload),
        )
        .await
        .map_err(|_| AppError::Other("call signal timed out".into()))?
    }

    /// End the call `call_id` if it is the active call and `condition` holds for it.
    /// Returns false if no call was ended.
    pub(super) async fn end_call_if(
        &self,
        call_id: &str,
        condition: impl FnOnce(&ActiveCall) -> bool,
        state: CallState,
        notice: PeerNotice,
    ) -> bool {
        let call = {
            let mut lock = self.active_call.lock().await;
            match lock.as_ref() {
                Some(call) if call.call_id == call_id && condition(call) => lock.take(),
                _ => None,
            }
        };
        match call {
            Some(call) => {
                self.finish_call(call, state, notice).await;
                true
            }
            None => false,
        }
    }

    /// End the call `call_id` if it is the active call. Returns false otherwise.
    pub(super) async fn end_call(
        &self,
        call_id: &str,
        state: CallState,
        notice: PeerNotice,
    ) -> bool {
        self.end_call_if(call_id, |_| true, state, notice).await
    }

    /// Release everything a call that was removed from `active_call` holds, and report
    /// its end: cancel the call token (stops the phase timeout, the audio session and
    /// the microphone thread), close the audio devices, restore the Android audio
    /// routing, emit `state` to the frontend and send `notice` to the peer.
    pub(super) async fn finish_call(&self, call: ActiveCall, state: CallState, notice: PeerNotice) {
        call.cancel.cancel();

        let capture = call.capture.clone();
        let playback = call.playback.clone();
        let closed = tokio::task::spawn_blocking(move || {
            lock(&capture).take();
            lock(&playback).take();
            android::restore_default_routing();
        })
        .await;
        if let Err(e) = closed {
            log::warn!("[call] closing audio devices failed: {e}");
        }

        self.emit_call_event(&call.call_id, &call.peer_pubkey, state);
        log::info!("[call] call {} ended ({state:?})", short_id(&call.call_id));

        let payload = match notice {
            PeerNotice::None => return,
            PeerNotice::Hangup => DmPayload::CallHangup {
                call_id: call.call_id.clone(),
            },
            PeerNotice::Reject => DmPayload::CallReject {
                call_id: call.call_id.clone(),
            },
        };
        if let Err(e) = self
            .send_signal(&call.peer_pubkey, call.peer_device, payload)
            .await
        {
            log::warn!(
                "[call] failed to tell {} that the call ended: {e}",
                short_id(&call.peer_pubkey)
            );
        }
    }

    /// End the call with `state` and `notice` if `token` is not cancelled within `after`.
    /// `token` is the phase timeout token, which the next phase cancels while holding
    /// the `active_call` lock.
    pub(super) fn spawn_phase_timeout(
        &self,
        call_id: &str,
        token: CancellationToken,
        after: Duration,
        state: CallState,
        notice: PeerNotice,
    ) {
        let handler = self.clone();
        let call_id = call_id.to_string();
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = tokio::time::sleep(after) => {
                    let ended = handler
                        .end_call_if(&call_id, |_| !token.is_cancelled(), state, notice)
                        .await;
                    if ended {
                        log::info!("[call] call {} timed out", short_id(&call_id));
                    }
                }
            }
        });
    }

    /// Switch the input (microphone) device mid-call. Saves the preference
    /// and immediately rebuilds the capture stream on the new device.
    pub async fn switch_input_device(&self, name: &str) -> Result<(), AppError> {
        self.storage
            .set_preference(preferences::AUDIO_INPUT_DEVICE, name)
            .await?;

        let capture = match &*self.active_call.lock().await {
            Some(call) => call.capture.clone(),
            None => return Ok(()),
        };
        let device_name = Some(name.to_string()).filter(|n| !n.is_empty());
        tokio::task::spawn_blocking(move || match lock(&capture).as_mut() {
            Some(capture) => capture.switch_device(device_name.as_deref()),
            None => Ok(()),
        })
        .await
        .map_err(|e| AppError::Other(format!("failed to switch input device: {e}")))?
        .map_err(|e| AppError::Other(format!("failed to switch input device: {e}")))
    }

    /// Switch the output (speaker) device mid-call. Saves the preference
    /// and immediately rebuilds the playback stream on the new device.
    pub async fn switch_output_device(&self, name: &str) -> Result<(), AppError> {
        self.storage
            .set_preference(preferences::AUDIO_OUTPUT_DEVICE, name)
            .await?;

        let playback = match &*self.active_call.lock().await {
            Some(call) => call.playback.clone(),
            None => return Ok(()),
        };
        let device_name = Some(name.to_string()).filter(|n| !n.is_empty());
        tokio::task::spawn_blocking(move || match lock(&playback).as_mut() {
            Some(playback) => playback.switch_device(device_name.as_deref()),
            None => Ok(()),
        })
        .await
        .map_err(|e| AppError::Other(format!("failed to switch output device: {e}")))?
        .map_err(|e| AppError::Other(format!("failed to switch output device: {e}")))
    }

    /// Rebuild the live cpal capture and playback streams on the default
    /// device. Called after Android audio route changes -- Android kills
    /// the old AAudio streams when the communication device changes, so
    /// we must build new ones.
    pub async fn rebuild_streams(&self) {
        let (capture, playback) = match &*self.active_call.lock().await {
            Some(call) => (call.capture.clone(), call.playback.clone()),
            None => return,
        };
        let rebuilt = tokio::task::spawn_blocking(move || {
            if let Some(capture) = lock(&capture).as_mut()
                && let Err(e) = capture.switch_device(None)
            {
                log::warn!("[call] failed to rebuild capture stream: {e}");
            }
            if let Some(playback) = lock(&playback).as_mut()
                && let Err(e) = playback.switch_device(None)
            {
                log::warn!("[call] failed to rebuild playback stream: {e}");
            }
        })
        .await;
        match rebuilt {
            Ok(()) => log::info!("[call] rebuilt audio streams after route change"),
            Err(e) => log::warn!("[call] rebuilding audio streams failed: {e}"),
        }
    }

    /// Toggle mute on the current call. Returns the new mute state.
    pub async fn toggle_mute(&self) -> Result<bool, AppError> {
        let lock = self.active_call.lock().await;
        let call = lock
            .as_ref()
            .ok_or(AppError::Other("no active call".into()))?;
        let muted = !call.muted.fetch_xor(true, Ordering::Relaxed);
        log::info!(
            "[call] {} call {}",
            if muted { "muted" } else { "unmuted" },
            short_id(&call.call_id)
        );
        Ok(muted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_call_phase_follows_direction_and_enter_phase_replaces_timeout() {
        let mut outgoing = ActiveCall::new("id", "peer", None, true);
        assert_eq!(outgoing.phase, CallPhase::Ringing);
        let incoming = ActiveCall::new("id", "peer", None, false);
        assert_eq!(incoming.phase, CallPhase::Incoming);

        let ring_timeout = outgoing.phase_timeout.clone();
        let connect_timeout = outgoing.enter_phase(CallPhase::Connecting);
        assert!(ring_timeout.is_cancelled());
        assert!(!connect_timeout.is_cancelled());
        assert_eq!(outgoing.phase, CallPhase::Connecting);

        // Ending the call cancels the timeout of the current phase.
        outgoing.cancel.cancel();
        assert!(connect_timeout.is_cancelled());
    }
}
