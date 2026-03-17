use crate::audio::{
    AudioCapture, AudioPlayback, OpusDecoder, OpusEncoder, SAMPLES_PER_FRAME, TAG_NORMAL,
    read_audio_frame, write_audio_frame,
};
use crate::dm::DmHandler;
use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_social_types::{CALL_ALPN, CallEvent, CallState, DmPayload, short_id};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Active call state tracked by the handler.
struct ActiveCall {
    call_id: String,
    peer_pubkey: String,
    cancel: CancellationToken,
    muted: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub struct CallHandler {
    storage: Arc<Storage>,
    #[allow(dead_code)] // will be used for caller authentication
    identity: SharedIdentity,
    endpoint: Endpoint,
    dm: DmHandler,
    app_handle: AppHandle,
    active_call: Arc<Mutex<Option<ActiveCall>>>,
}

impl CallHandler {
    pub fn new(
        storage: Arc<Storage>,
        identity: SharedIdentity,
        endpoint: Endpoint,
        dm: DmHandler,
        app_handle: AppHandle,
    ) -> Self {
        Self {
            storage,
            identity,
            endpoint,
            dm,
            app_handle,
            active_call: Arc::new(Mutex::new(None)),
        }
    }

    fn emit_call_event(&self, call_id: &str, peer_pubkey: &str, state: CallState) {
        let _ = self.app_handle.emit(
            "call-state",
            CallEvent {
                call_id: call_id.to_string(),
                peer_pubkey: peer_pubkey.to_string(),
                state,
            },
        );
    }

    /// Initiate an outgoing call to a peer. Sends a CallOffer via the DM ratchet,
    /// then opens a QUIC connection on CALL_ALPN for audio.
    pub async fn start_call(&self, peer_pubkey: &str) -> Result<String, AppError> {
        // Reject if already in a call
        {
            let lock = self.active_call.lock().await;
            if lock.is_some() {
                return Err(AppError::Other("already in a call".into()));
            }
        }

        let call_id = crate::util::generate_id();
        log::info!(
            "[call] starting call {} to {}",
            short_id(&call_id),
            short_id(peer_pubkey)
        );

        // Send call offer via E2E encrypted DM channel
        let offer = DmPayload::CallOffer {
            call_id: call_id.clone(),
            video: false,
        };
        self.dm.send_signal(peer_pubkey, offer).await?;

        let cancel = CancellationToken::new();
        {
            let mut lock = self.active_call.lock().await;
            *lock = Some(ActiveCall {
                call_id: call_id.clone(),
                peer_pubkey: peer_pubkey.to_string(),
                cancel: cancel.clone(),
                muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            });
        }

        self.emit_call_event(&call_id, peer_pubkey, CallState::Ringing);

        // Spawn a timeout: if no answer within 30s, cancel
        let handler = self.clone();
        let cid = call_id.clone();
        let pk = peer_pubkey.to_string();
        let timeout_cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = timeout_cancel.cancelled() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                    log::info!("[call] timeout waiting for answer on {}", short_id(&cid));
                    handler.emit_call_event(&cid, &pk, CallState::Failed);
                    let mut lock = handler.active_call.lock().await;
                    if lock.as_ref().is_some_and(|c| c.call_id == cid) {
                        *lock = None;
                    }
                }
            }
        });

        Ok(call_id)
    }

    /// Called when we receive a CallAnswer from the peer (via DM handler).
    /// Establishes the audio QUIC connection and starts streaming.
    pub async fn on_call_answered(&self, call_id: &str, peer_pubkey: &str) {
        let cancel = {
            let lock = self.active_call.lock().await;
            match &*lock {
                Some(call) if call.call_id == call_id => call.cancel.clone(),
                _ => {
                    log::warn!(
                        "[call] received answer for unknown call {}",
                        short_id(call_id)
                    );
                    return;
                }
            }
        };
        // Cancel the timeout
        cancel.cancel();

        self.emit_call_event(call_id, peer_pubkey, CallState::Active);

        // Resolve peer transport and connect
        let node_ids = self
            .storage
            .get_peer_transport_node_ids(peer_pubkey)
            .await
            .unwrap_or_default();
        let target: Option<EndpointId> = node_ids.iter().find_map(|id| id.parse().ok());

        let Some(target) = target else {
            log::error!("[call] no transport NodeId for {}", short_id(peer_pubkey));
            self.emit_call_event(call_id, peer_pubkey, CallState::Failed);
            *self.active_call.lock().await = None;
            return;
        };

        let conn = match self
            .endpoint
            .connect(EndpointAddr::from(target), CALL_ALPN)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                log::error!("[call] failed to connect for audio: {e}");
                self.emit_call_event(call_id, peer_pubkey, CallState::Failed);
                *self.active_call.lock().await = None;
                return;
            }
        };

        log::info!(
            "[call] audio connection established for {}",
            short_id(call_id)
        );
        let handler = self.clone();
        let cid = call_id.to_string();
        let pk = peer_pubkey.to_string();
        tokio::spawn(async move {
            handler.run_audio_session(conn, &cid, &pk).await;
        });
    }

    /// Called when we receive a CallOffer from a peer (via DM handler).
    pub async fn on_call_offer(&self, call_id: &str, peer_pubkey: &str) {
        {
            let lock = self.active_call.lock().await;
            if lock.is_some() {
                // Already in a call, auto-reject
                log::info!(
                    "[call] auto-rejecting offer {} (already in call)",
                    short_id(call_id)
                );
                let reject = DmPayload::CallReject {
                    call_id: call_id.to_string(),
                };
                let _ = self.dm.send_signal(peer_pubkey, reject).await;
                return;
            }
        }

        let cancel = CancellationToken::new();
        {
            let mut lock = self.active_call.lock().await;
            *lock = Some(ActiveCall {
                call_id: call_id.to_string(),
                peer_pubkey: peer_pubkey.to_string(),
                cancel: cancel.clone(),
                muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            });
        }

        self.emit_call_event(call_id, peer_pubkey, CallState::Incoming);

        // Timeout: auto-reject after 30s if not answered
        let handler = self.clone();
        let cid = call_id.to_string();
        let pk = peer_pubkey.to_string();
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                    log::info!("[call] incoming call {} timed out", short_id(&cid));
                    handler.emit_call_event(&cid, &pk, CallState::Ended);
                    let mut lock = handler.active_call.lock().await;
                    if lock.as_ref().is_some_and(|c| c.call_id == cid) {
                        let reject = DmPayload::CallReject { call_id: cid };
                        let _ = handler.dm.send_signal(&pk, reject).await;
                        *lock = None;
                    }
                }
            }
        });
    }

    /// Accept an incoming call. Sends CallAnswer via DM and waits for the
    /// caller to open the CALL_ALPN connection (handled by ProtocolHandler::accept).
    pub async fn accept_call(&self, call_id: &str) -> Result<(), AppError> {
        let peer_pubkey = {
            let lock = self.active_call.lock().await;
            match &*lock {
                Some(call) if call.call_id == call_id => {
                    call.cancel.cancel(); // cancel timeout
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
        self.dm.send_signal(&peer_pubkey, answer).await?;
        self.emit_call_event(call_id, &peer_pubkey, CallState::Active);
        Ok(())
    }

    /// Reject an incoming call or cancel an outgoing call.
    pub async fn reject_call(&self, call_id: &str) -> Result<(), AppError> {
        let peer_pubkey = {
            let mut lock = self.active_call.lock().await;
            match lock.take() {
                Some(call) if call.call_id == call_id => {
                    call.cancel.cancel();
                    call.peer_pubkey
                }
                Some(other) => {
                    // Put it back, wrong call
                    let pk = other.peer_pubkey.clone();
                    *lock = Some(other);
                    return Err(AppError::Other(format!(
                        "active call is {}, not {call_id}",
                        short_id(&pk)
                    )));
                }
                None => return Err(AppError::Other("no active call".into())),
            }
        };

        let reject = DmPayload::CallReject {
            call_id: call_id.to_string(),
        };
        let _ = self.dm.send_signal(&peer_pubkey, reject).await;
        self.emit_call_event(call_id, &peer_pubkey, CallState::Ended);
        Ok(())
    }

    /// Hang up the current call.
    pub async fn hangup(&self) -> Result<(), AppError> {
        let call = {
            let mut lock = self.active_call.lock().await;
            lock.take()
        };
        let Some(call) = call else {
            return Err(AppError::Other("no active call".into()));
        };

        call.cancel.cancel();
        let hangup = DmPayload::CallHangup {
            call_id: call.call_id.clone(),
        };
        let _ = self.dm.send_signal(&call.peer_pubkey, hangup).await;
        self.emit_call_event(&call.call_id, &call.peer_pubkey, CallState::Ended);
        log::info!("[call] hung up {}", short_id(&call.call_id));
        Ok(())
    }

    /// Toggle mute on the current call. Returns the new mute state.
    pub async fn toggle_mute(&self) -> Result<bool, AppError> {
        let lock = self.active_call.lock().await;
        let call = lock
            .as_ref()
            .ok_or(AppError::Other("no active call".into()))?;
        let was_muted = call.muted.load(std::sync::atomic::Ordering::Relaxed);
        call.muted
            .store(!was_muted, std::sync::atomic::Ordering::Relaxed);
        log::info!(
            "[call] {} call {}",
            if was_muted { "unmuted" } else { "muted" },
            short_id(&call.call_id)
        );
        Ok(!was_muted)
    }

    /// Called when peer sends CallReject or CallHangup.
    pub async fn on_call_ended(&self, call_id: &str) {
        let mut lock = self.active_call.lock().await;
        if let Some(call) = &*lock
            && call.call_id == call_id
        {
            call.cancel.cancel();
            let pk = call.peer_pubkey.clone();
            *lock = None;
            drop(lock);
            self.emit_call_event(call_id, &pk, CallState::Ended);
        }
    }

    /// Run a bidirectional audio session over an established QUIC connection.
    /// Spawns capture + send and receive + playback tasks.
    async fn run_audio_session(&self, conn: Connection, call_id: &str, peer_pubkey: &str) {
        let (call_cancel, muted) = {
            let lock = self.active_call.lock().await;
            match &*lock {
                Some(c) if c.call_id == call_id => (c.cancel.clone(), c.muted.clone()),
                _ => return,
            }
        };

        // Open a bi-stream: we send audio on send, receive on recv
        let (send, recv) = match conn.open_bi().await {
            Ok(pair) => pair,
            Err(e) => {
                log::error!("[call] failed to open bi-stream: {e}");
                self.emit_call_event(call_id, peer_pubkey, CallState::Failed);
                *self.active_call.lock().await = None;
                return;
            }
        };

        let session_cancel = call_cancel.child_token();

        // Spawn send task (capture -> encode -> send)
        let send_cancel = session_cancel.clone();
        let send_task = tokio::spawn(Self::send_audio_loop(send, send_cancel, muted));

        // Spawn receive task (receive -> decode -> playback)
        let recv_cancel = session_cancel.clone();
        let recv_task = tokio::spawn(Self::recv_audio_loop(recv, recv_cancel));

        // Wait for either task to finish or call to be cancelled
        tokio::select! {
            _ = session_cancel.cancelled() => {
                log::info!("[call] session cancelled for {}", short_id(call_id));
            }
            result = send_task => {
                if let Err(e) = result {
                    log::error!("[call] send task panicked: {e}");
                }
            }
            result = recv_task => {
                if let Err(e) = result {
                    log::error!("[call] recv task panicked: {e}");
                }
            }
        }

        conn.close(0u32.into(), b"call ended");
        log::info!("[call] audio session ended for {}", short_id(call_id));

        // Clean up if we haven't already
        let mut lock = self.active_call.lock().await;
        if lock.as_ref().is_some_and(|c| c.call_id == call_id) {
            let pk = lock.as_ref().unwrap().peer_pubkey.clone();
            *lock = None;
            drop(lock);
            self.emit_call_event(call_id, &pk, CallState::Ended);
        }
    }

    /// Capture audio from mic, encode to Opus, send over QUIC.
    async fn send_audio_loop(
        mut send: iroh::endpoint::SendStream,
        cancel: CancellationToken,
        muted: Arc<std::sync::atomic::AtomicBool>,
    ) {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<f32>>(32);

        let _capture = match AudioCapture::start(tx) {
            Ok(c) => c,
            Err(e) => {
                log::error!("[call] failed to start audio capture: {e}");
                return;
            }
        };

        let mut encoder = match OpusEncoder::new() {
            Ok(e) => e,
            Err(e) => {
                log::error!("[call] failed to create Opus encoder: {e}");
                return;
            }
        };

        let mut seq: u32 = 0;
        let mut timestamp: u32 = 0;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                samples = rx.recv() => {
                    let Some(samples) = samples else { break };
                    // When muted, feed silence to the encoder to maintain timing
                    let samples = if muted.load(std::sync::atomic::Ordering::Relaxed) {
                        vec![0.0f32; samples.len()]
                    } else {
                        samples
                    };
                    let packets = encoder.push_samples(&samples);
                    for packet in packets {
                        if let Err(e) = write_audio_frame(
                            &mut send, seq, timestamp, TAG_NORMAL, &packet,
                        ).await {
                            log::error!("[call] send error: {e}");
                            return;
                        }
                        seq = seq.wrapping_add(1);
                        timestamp = timestamp.wrapping_add(SAMPLES_PER_FRAME as u32);
                    }
                }
            }
        }

        let _ = send.finish();
        log::info!("[call] send loop ended (sent {seq} frames)");
    }

    /// Receive Opus frames from QUIC, decode, play back.
    async fn recv_audio_loop(mut recv: iroh::endpoint::RecvStream, cancel: CancellationToken) {
        let mut decoder = match OpusDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                log::error!("[call] failed to create Opus decoder: {e}");
                return;
            }
        };

        let (mut prod, _playback) = match AudioPlayback::start() {
            Ok(p) => p,
            Err(e) => {
                log::error!("[call] failed to start audio playback: {e}");
                return;
            }
        };

        let mut frames_received: u32 = 0;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                result = read_audio_frame(&mut recv) => {
                    match result {
                        Ok(Some((_seq, _ts, _tag, payload))) => {
                            match decoder.decode(&payload) {
                                Ok(samples) => {
                                    let pushed = prod.push(&samples);
                                    if pushed < samples.len() {
                                        log::debug!("[call] ring buffer full, dropped {} samples", samples.len() - pushed);
                                    }
                                }
                                Err(e) => {
                                    log::warn!("[call] decode error: {e}");
                                }
                            }
                            frames_received += 1;
                        }
                        Ok(None) => {
                            log::info!("[call] remote stream closed");
                            break;
                        }
                        Err(e) => {
                            log::error!("[call] recv error: {e}");
                            break;
                        }
                    }
                }
            }
        }

        log::info!("[call] recv loop ended ({frames_received} frames received)");
    }
}

impl std::fmt::Debug for CallHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallHandler").finish_non_exhaustive()
    }
}

impl ProtocolHandler for CallHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        let remote_str = remote.to_string();
        log::info!(
            "[call] incoming audio connection from {}",
            short_id(&remote_str)
        );

        // Resolve transport NodeId to master pubkey
        let peer_pubkey = self
            .storage
            .get_master_pubkey_for_transport(&remote_str)
            .await
            .unwrap_or_else(|| remote_str.clone());

        // Check we have an active call with this peer
        let call_id = {
            let lock = self.active_call.lock().await;
            match &*lock {
                Some(call) if call.peer_pubkey == peer_pubkey => call.call_id.clone(),
                _ => {
                    log::warn!(
                        "[call] rejecting unexpected audio connection from {}",
                        short_id(&peer_pubkey)
                    );
                    return Err(AcceptError::from_err(std::io::Error::other(
                        "no active call with this peer",
                    )));
                }
            }
        };

        log::info!(
            "[call] accepting audio stream for call {}",
            short_id(&call_id)
        );

        // The caller opened a bi-stream; accept it for audio data
        let (send, recv) = conn.accept_bi().await?;

        let (session_cancel, muted) = {
            let lock = self.active_call.lock().await;
            let call = lock.as_ref().unwrap();
            (call.cancel.child_token(), call.muted.clone())
        };

        let send_cancel = session_cancel.clone();
        let recv_cancel = session_cancel.clone();

        let send_task = tokio::spawn(Self::send_audio_loop(send, send_cancel, muted));
        let recv_task = tokio::spawn(Self::recv_audio_loop(recv, recv_cancel));

        tokio::select! {
            _ = session_cancel.cancelled() => {}
            _ = send_task => {}
            _ = recv_task => {}
        }

        // Clean up
        let mut lock = self.active_call.lock().await;
        if lock.as_ref().is_some_and(|c| c.call_id == call_id) {
            let pk = lock.as_ref().unwrap().peer_pubkey.clone();
            *lock = None;
            drop(lock);
            self.emit_call_event(&call_id, &pk, CallState::Ended);
        }

        conn.closed().await;
        Ok(())
    }
}
