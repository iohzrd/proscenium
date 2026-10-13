use super::auth::{self, AuthResult, ListenerAuthState};
use super::command::ActorSender;
use super::control::{ControlSigner, verify_hello};
use super::fanout::Fanout;
use super::speaker_mixer::SpeakerMixerHandle;
use super::{CLOSE_BANNED, CLOSE_NORMAL, CONN_TYPE_LISTENER, CONN_TYPE_SPEAKER, StageCommand};
use crate::audio::{
    AudioPlayback, MicConfig, OpusDecoder, OpusEncoder, PCM_CHANNEL_CAPACITY, SAMPLES_PER_FRAME,
    TAG_CHECKPOINT, TAG_NORMAL, WireFrame, read_audio_frame, spawn_mic_thread, write_audio_frame,
};
use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, PublicKey};
use proscenium_types::{STAGE_ALPN, SignedStageControl, StageEvent, short_id};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Timeout for one handshake step: opening or accepting a stream, reading or writing
/// the hello.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for establishing a QUIC connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A stream that delivers no frame for this long is treated as failed.
pub(super) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest hello (a signed `Presence` as JSON) accepted on an audio connection.
const MAX_HELLO_LEN: usize = 4096;

pub(super) const BACKOFF_INITIAL: Duration = Duration::from_secs(2);
pub(super) const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Consecutive failed sessions on a relay after which a listener returns to the host.
const RELAY_MAX_FAILURES: u32 = 2;
/// Consecutive undecodable frames after which the host closes a speaker connection.
const MAX_DECODE_ERRORS: u32 = 50;

// ---- Connection hello ---------------------------------------------------

/// Type of an audio connection, sent by the dialing side as the first byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnKind {
    Speaker,
    Listener,
}

/// An incoming audio connection whose hello was verified: the remote NodeId is
/// `node_id` and a key delegated by `master_pubkey` signed a `Presence` naming it.
pub(crate) struct IncomingAudio {
    pub(super) conn: Connection,
    pub(super) kind: ConnKind,
    pub(super) master_pubkey: String,
    pub(super) node_id: String,
    pub(super) send: SendStream,
    pub(super) recv: RecvStream,
}

/// Dial `node_id`, open the audio bi-stream and send the hello:
/// `[conn type:1][hello length:u16 BE][signed Presence JSON]`.
pub(super) async fn open_audio_connection(
    endpoint: &Endpoint,
    node_id: EndpointId,
    conn_type: u8,
    signer: &ControlSigner,
) -> Result<(Connection, SendStream, RecvStream), String> {
    let conn = tokio::time::timeout(
        CONNECT_TIMEOUT,
        endpoint.connect(EndpointAddr::from(node_id), STAGE_ALPN),
    )
    .await
    .map_err(|_| "connect timed out".to_string())?
    .map_err(|e| format!("connect failed: {e}"))?;

    let (mut send, recv) = tokio::time::timeout(IO_TIMEOUT, conn.open_bi())
        .await
        .map_err(|_| "opening the stream timed out".to_string())?
        .map_err(|e| format!("opening the stream failed: {e}"))?;

    let hello = serde_json::to_vec(&signer.presence()).map_err(|e| e.to_string())?;
    let len = u16::try_from(hello.len()).map_err(|_| "hello too large".to_string())?;
    let mut frame = Vec::with_capacity(3 + hello.len());
    frame.push(conn_type);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&hello);
    tokio::time::timeout(IO_TIMEOUT, send.write_all(&frame))
        .await
        .map_err(|_| "writing the hello timed out".to_string())?
        .map_err(|e| format!("writing the hello failed: {e}"))?;

    Ok((conn, send, recv))
}

/// Accept the audio bi-stream of an incoming connection and read its hello.
/// Checks the signature, delegation and stage id of the `Presence`, and that it names
/// the NodeId QUIC authenticated for this connection.
pub(super) async fn accept_audio_connection(
    conn: Connection,
    stage_id: &str,
) -> Result<IncomingAudio, String> {
    let remote = conn.remote_id().to_string();

    let (send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_| "accepting the stream timed out".to_string())?
        .map_err(|e| format!("accepting the stream failed: {e}"))?;

    let mut header = [0u8; 3];
    tokio::time::timeout(IO_TIMEOUT, recv.read_exact(&mut header))
        .await
        .map_err(|_| "reading the hello header timed out".to_string())?
        .map_err(|e| format!("reading the hello header failed: {e}"))?;
    let [conn_type, len_hi, len_lo] = header;
    let kind = match conn_type {
        CONN_TYPE_SPEAKER => ConnKind::Speaker,
        CONN_TYPE_LISTENER => ConnKind::Listener,
        other => return Err(format!("unknown connection type {other:#x}")),
    };
    let len = usize::from(u16::from_be_bytes([len_hi, len_lo]));
    if len == 0 || len > MAX_HELLO_LEN {
        return Err(format!("hello length out of range: {len}"));
    }
    let mut hello = vec![0u8; len];
    tokio::time::timeout(IO_TIMEOUT, recv.read_exact(&mut hello))
        .await
        .map_err(|_| "reading the hello timed out".to_string())?
        .map_err(|e| format!("reading the hello failed: {e}"))?;

    let signed: SignedStageControl =
        serde_json::from_slice(&hello).map_err(|e| format!("invalid hello: {e}"))?;
    let presence = verify_hello(&signed, stage_id)?;
    if presence.node_id != remote {
        return Err("hello names another node".to_string());
    }

    Ok(IncomingAudio {
        conn,
        kind,
        master_pubkey: presence.master_pubkey,
        node_id: remote,
        send,
        recv,
    })
}

// ---- Host: speaker connections ------------------------------------------

/// Byte the host writes on the bi-stream of a speaker connection it accepted.
/// The speaker opens its microphone only after it read this byte.
const SPEAKER_ACCEPTED: u8 = 0x01;

/// Tell a speaker that its connection was accepted, then finish the stream.
/// The host sends nothing else on the bi-stream; speakers receive uni-streams.
pub(super) fn spawn_speaker_accepted(mut send: SendStream) {
    tokio::spawn(async move {
        let _ = tokio::time::timeout(IO_TIMEOUT, send.write_all(&[SPEAKER_ACCEPTED])).await;
        let _ = send.finish();
    });
}

/// Open a uni-stream on `conn` and forward the frames of `fanout` to it.
/// The stream ends when `cancel` is cancelled, a write fails, or `fanout` is dropped.
pub(super) fn spawn_forward(fanout: Arc<Fanout>, conn: Connection, cancel: CancellationToken) {
    tokio::spawn(async move {
        let opened = tokio::select! {
            _ = cancel.cancelled() => return,
            opened = tokio::time::timeout(IO_TIMEOUT, conn.open_uni()) => opened,
        };
        match opened {
            Ok(Ok(uni)) => {
                fanout.add_subscriber(uni, cancel);
            }
            Ok(Err(e)) => log::debug!("[stage-host] open_uni failed: {e}"),
            Err(_) => log::warn!(
                "[stage-host] open_uni to {} timed out",
                short_id(&conn.remote_id().to_string())
            ),
        }
    });
}

/// Host side of one speaker connection: receive Opus frames, forward each raw frame to
/// the speaker's fanout (for the other speakers) and send the decoded PCM to the mixer.
/// Frames received while `muted` is set are discarded.
/// Sends `SpeakerConnClosed` to the actor when the loop ends.
#[allow(clippy::too_many_arguments)]
pub(super) async fn speaker_recv_loop(
    mut recv: RecvStream,
    node_id: String,
    conn_id: u64,
    pcm_tx: mpsc::Sender<Vec<f32>>,
    sfu_fanout: Arc<Fanout>,
    muted: Arc<AtomicBool>,
    cancel: CancellationToken,
    cmd_tx: ActorSender,
) {
    let mut frames: u32 = 0;
    match OpusDecoder::new() {
        Ok(mut decoder) => {
            let mut decode_errors: u32 = 0;
            loop {
                let frame = tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = tokio::time::timeout(STREAM_IDLE_TIMEOUT, read_audio_frame(&mut recv)) => {
                        match result {
                            Ok(Ok(Some(frame))) => frame,
                            Ok(Ok(None)) => break,
                            Ok(Err(e)) => {
                                log::debug!("[stage-host] recv error from {}: {e}", short_id(&node_id));
                                break;
                            }
                            Err(_) => {
                                log::info!("[stage-host] speaker {} sent no frame, closing", short_id(&node_id));
                                break;
                            }
                        }
                    }
                };
                if muted.load(Ordering::Relaxed) {
                    continue;
                }
                match decoder.decode(&frame.payload) {
                    Ok(samples) => {
                        decode_errors = 0;
                        sfu_fanout.send_frame(
                            frame.seq,
                            frame.timestamp,
                            TAG_NORMAL,
                            frame.payload,
                        );
                        if let Err(mpsc::error::TrySendError::Closed(_)) = pcm_tx.try_send(samples)
                        {
                            break;
                        }
                        frames = frames.wrapping_add(1);
                    }
                    Err(e) => {
                        decode_errors += 1;
                        if decode_errors > MAX_DECODE_ERRORS {
                            log::warn!(
                                "[stage-host] closing speaker {}: {MAX_DECODE_ERRORS} undecodable frames ({e})",
                                short_id(&node_id)
                            );
                            break;
                        }
                    }
                }
            }
        }
        Err(e) => log::error!(
            "[stage-host] failed to create decoder for {}: {e}",
            short_id(&node_id)
        ),
    }

    log::debug!(
        "[stage-host] speaker {} receive loop ended ({frames} frames)",
        short_id(&node_id)
    );
    cmd_tx
        .send(StageCommand::SpeakerConnClosed { node_id, conn_id })
        .await;
}

/// Receive mix-minus PCM from the mixer and play it locally (host only).
///
/// The mixer sends only the contributions of other speakers; the host's own
/// voice is excluded, so the host does not hear itself.
pub(super) async fn run_host_playback(
    mut rx: mpsc::Receiver<Vec<f32>>,
    far_end_tx: mpsc::Sender<Vec<f32>>,
    cancel: CancellationToken,
) {
    let (mut prod, _playback) = match AudioPlayback::start_blocking(None).await {
        Ok(p) => p,
        Err(e) => {
            log::error!("[stage-host] failed to start playback: {e}");
            return;
        }
    };

    let mut frames: u32 = 0;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            pcm = rx.recv() => {
                let Some(samples) = pcm else { break };
                let _ = far_end_tx.try_send(samples.clone());
                prod.push(&samples);
                frames = frames.wrapping_add(1);
            }
        }
    }
    log::info!("[stage-host] host playback stopped ({frames} frames)");
}

// ---- Listener pipeline --------------------------------------------------

/// Parameters of the listener audio pipeline.
pub(super) struct ListenerConfig {
    pub(super) endpoint: Endpoint,
    pub(super) stage_id: String,
    /// Node to receive the mixed stream from: the host or a relay.
    pub(super) upstream_node_id: String,
    pub(super) host_node_id: String,
    /// Key that signs the checkpoints of the mixed stream.
    pub(super) host_signing_pubkey: PublicKey,
    pub(super) signer: Arc<ControlSigner>,
    /// Highest checkpoint sequence number verified in this stage session.
    pub(super) checkpoint_floor: Arc<AtomicU64>,
    pub(super) app_handle: AppHandle,
    pub(super) cmd_tx: ActorSender,
    pub(super) cancel: CancellationToken,
}

/// How one audio session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
    /// Connecting failed, the serving node did not accept the connection, or the
    /// session ended before it carried audio.
    Failed,
    /// The session ended after it had carried audio (listener: verified audio).
    EndedAfterAudio,
    /// Listener only: the stream failed authentication.
    AuthFailed,
    /// The serving node closed the connection because this identity is banned.
    Banned,
}

/// Reason the remote closed `conn`, if it was the application close code `code`.
fn closed_with_code(conn: &Connection, code: u32) -> bool {
    matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(close)) if close.error_code == code.into()
    )
}

/// Connect to `upstream_node_id`, receive the mixed stream, verify it, decode and play.
/// Reconnects with exponential backoff; the delay returns to `BACKOFF_INITIAL` after a
/// session that delivered verified audio.
///
/// A relay that fails authentication, or fails `RELAY_MAX_FAILURES` sessions in a row,
/// is reported to the actor with `UpstreamFailed` and is not dialed again: the pipeline
/// continues with the host.
pub(super) async fn start_listener_pipeline(config: ListenerConfig) {
    let mut upstream = config.upstream_node_id.clone();
    let mut backoff = BACKOFF_INITIAL;
    let mut relay_failures: u32 = 0;

    loop {
        let outcome = run_listener_once(&config, &upstream).await;
        if config.cancel.is_cancelled() {
            break;
        }

        let on_relay = upstream != config.host_node_id;
        let abandon_relay = match outcome {
            SessionEnd::Banned => {
                log::info!("[stage-listener] rejected as banned, stopping");
                break;
            }
            SessionEnd::EndedAfterAudio => {
                backoff = BACKOFF_INITIAL;
                relay_failures = 0;
                false
            }
            SessionEnd::AuthFailed => on_relay,
            SessionEnd::Failed => {
                relay_failures += 1;
                on_relay && relay_failures >= RELAY_MAX_FAILURES
            }
        };

        if abandon_relay {
            log::warn!(
                "[stage-listener] relay {} failed, returning to the host",
                short_id(&upstream)
            );
            config
                .cmd_tx
                .send(StageCommand::UpstreamFailed {
                    stage_id: config.stage_id.clone(),
                    node_id: upstream.clone(),
                })
                .await;
            upstream = config.host_node_id.clone();
            backoff = BACKOFF_INITIAL;
            relay_failures = 0;
            continue;
        }

        log::info!("[stage-listener] stream ended, reconnecting in {backoff:?}");
        tokio::select! {
            _ = config.cancel.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Single connection attempt for the listener audio pipeline.
async fn run_listener_once(config: &ListenerConfig, upstream_node_id: &str) -> SessionEnd {
    let Ok(upstream_id) = upstream_node_id.parse::<EndpointId>() else {
        log::error!("[stage-listener] invalid upstream node id");
        return SessionEnd::Failed;
    };

    let opened = tokio::select! {
        _ = config.cancel.cancelled() => return SessionEnd::Failed,
        opened = open_audio_connection(
            &config.endpoint,
            upstream_id,
            CONN_TYPE_LISTENER,
            &config.signer,
        ) => opened,
    };
    let (conn, _send, mut recv) = match opened {
        Ok(streams) => streams,
        Err(e) => {
            log::warn!(
                "[stage-listener] connection to {} failed: {e}",
                short_id(upstream_node_id)
            );
            return SessionEnd::Failed;
        }
    };
    let source_id = conn.remote_id().to_string();
    log::info!(
        "[stage-listener] connected to {}, starting playback",
        short_id(&source_id)
    );

    let mut auth = ListenerAuthState::new(
        config.host_signing_pubkey,
        config.stage_id.clone(),
        config.checkpoint_floor.load(Ordering::Relaxed),
    );

    let mut decoder = match OpusDecoder::new() {
        Ok(d) => d,
        Err(e) => {
            log::error!("[stage-listener] failed to create decoder: {e}");
            return SessionEnd::Failed;
        }
    };

    // Adaptive jitter buffer.
    //
    // The output callback writes silence until `target_frames` decoded frames are
    // buffered, at the start of the stream and again after each underrun
    // (`PlaybackProducer::set_prefill_frames`).
    //
    // Adaptation rules (applied once per decoded frame):
    //   - Underrun detected  -> target += 1 (up to JB_MAX_FRAMES)
    //   - DRIFT_INTERVAL consecutive frames with no underrun -> target -= 1 (down to JB_MIN_FRAMES)
    const JB_MIN_FRAMES: usize = 3; // 60 ms -- LAN / low-latency WAN
    const JB_INIT_FRAMES: usize = 4; // 80 ms -- conservative start
    const JB_MAX_FRAMES: usize = 10; // 200 ms -- ceiling for bad WAN
    const DRIFT_INTERVAL: usize = 250; // ~5 s at 20 ms/frame before drifting down

    let (mut prod, playback) = match AudioPlayback::start_blocking(None).await {
        Ok(p) => p,
        Err(e) => {
            log::error!("[stage-listener] failed to start playback: {e}");
            return SessionEnd::Failed;
        }
    };
    let mut target_frames: usize = JB_INIT_FRAMES;
    prod.set_prefill_frames(target_frames);
    let mut frames_since_adapt: usize = 0;

    // Sequence tracking for packet loss concealment.
    let mut last_seq: Option<u32> = None;
    let mut frames: u32 = 0;
    let mut verified_audio = false;
    let mut auth_failed = false;

    loop {
        let frame = tokio::select! {
            _ = config.cancel.cancelled() => break,
            result = tokio::time::timeout(STREAM_IDLE_TIMEOUT, read_audio_frame(&mut recv)) => {
                match result {
                    Ok(Ok(Some(frame))) => frame,
                    Ok(Ok(None)) => {
                        log::info!("[stage-listener] stream ended by {}", short_id(&source_id));
                        break;
                    }
                    Ok(Err(e)) => {
                        log::info!("[stage-listener] recv error: {e}");
                        break;
                    }
                    Err(_) => {
                        log::warn!("[stage-listener] no frame from {} within {STREAM_IDLE_TIMEOUT:?}", short_id(&source_id));
                        break;
                    }
                }
            }
        };
        let WireFrame {
            seq,
            timestamp,
            tag,
            payload,
        } = frame;

        // Split checkpoint prefix from Opus payload.
        let (checkpoint, opus) = if tag == TAG_CHECKPOINT {
            match auth::decode_checkpoint_payload(&payload) {
                Ok((checkpoint, opus)) => (Some(checkpoint), opus),
                Err(_) => (None, payload.as_slice()),
            }
        } else {
            (None, payload.as_slice())
        };

        // Verify the hash chain. A failure is final for this stream: stop playing it.
        match auth.verify_frame(seq, timestamp, tag, opus, checkpoint.as_ref()) {
            AuthResult::Failed(failure) => {
                log::warn!(
                    "[stage-listener] stream from {} failed authentication: {}",
                    short_id(&source_id),
                    failure.as_str()
                );
                let _ = config.app_handle.emit(
                    "stage-event",
                    StageEvent::AuthFailed {
                        source: source_id.clone(),
                        reason: failure.as_str().to_string(),
                    },
                );
                auth_failed = true;
                break;
            }
            AuthResult::Verified => {
                verified_audio = true;
                config
                    .checkpoint_floor
                    .fetch_max(auth.checkpoint_floor(), Ordering::Relaxed);
            }
            AuthResult::Ok | AuthResult::Unverified => {}
        }

        // Fill a detected gap with PLC frames before decoding this frame.
        if let Some(prev) = last_seq {
            let lost = seq.wrapping_sub(prev.wrapping_add(1));
            if lost > 0 && lost < 64 {
                log::debug!(
                    "[stage-listener] {lost} lost frame(s) before seq {seq}, inserting PLC"
                );
                for _ in 0..lost {
                    if let Ok(plc) = decoder.decode_loss() {
                        prod.push(&plc);
                    }
                }
            }
        }
        last_seq = Some(seq);

        // Decode and push into the jitter buffer.
        if let Ok(samples) = decoder.decode(opus) {
            prod.push(&samples);
            frames = frames.wrapping_add(1);

            let underruns = prod.drain_underruns();
            if underruns > 0 {
                if target_frames < JB_MAX_FRAMES {
                    target_frames += 1;
                    prod.set_prefill_frames(target_frames);
                    log::debug!(
                        "[stage-listener] underrun x{underruns}, target -> {}f ({}ms)",
                        target_frames,
                        target_frames * 20
                    );
                }
                frames_since_adapt = 0;
            } else {
                frames_since_adapt += 1;
                if frames_since_adapt >= DRIFT_INTERVAL && target_frames > JB_MIN_FRAMES {
                    target_frames -= 1;
                    prod.set_prefill_frames(target_frames);
                    frames_since_adapt = 0;
                    log::debug!(
                        "[stage-listener] drifting down, target -> {}f ({}ms)",
                        target_frames,
                        target_frames * 20
                    );
                }
            }
        }
    }

    drop(playback);
    let banned = closed_with_code(&conn, CLOSE_BANNED);
    conn.close(CLOSE_NORMAL.into(), b"listener left");
    log::info!("[stage-listener] playback stopped ({frames} frames decoded)");

    if banned {
        SessionEnd::Banned
    } else if auth_failed {
        SessionEnd::AuthFailed
    } else if verified_audio {
        SessionEnd::EndedAfterAudio
    } else {
        SessionEnd::Failed
    }
}

// ---- Speaker pipeline ---------------------------------------------------

/// Receive a single forwarded speaker stream from the host (via a QUIC uni-stream),
/// decode Opus to PCM, and push into the speaker mixer for this stream's slot.
/// Deregisters the stream from the mixer on exit.
async fn speaker_stream_recv(
    mut recv: RecvStream,
    stream_id: u32,
    pcm_tx: mpsc::Sender<Vec<f32>>,
    mixer: SpeakerMixerHandle,
    cancel: CancellationToken,
) {
    let mut frames: u32 = 0;
    match OpusDecoder::new() {
        Ok(mut decoder) => loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                result = read_audio_frame(&mut recv) => {
                    match result {
                        Ok(Some(frame)) => {
                            if let Ok(samples) = decoder.decode(&frame.payload) {
                                if let Err(mpsc::error::TrySendError::Closed(_)) = pcm_tx.try_send(samples) {
                                    break;
                                }
                                frames = frames.wrapping_add(1);
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        },
        Err(e) => log::error!("[stage-speaker] failed to create stream decoder: {e}"),
    }

    mixer.remove_stream(stream_id).await;
    log::debug!("[stage-speaker] stream recv stopped ({frames} frames)");
}

/// Encode mic PCM samples from the AEC output and send to host via a QUIC send stream.
/// Cancels `cancel` when the stream fails, which ends the speaker session.
async fn speaker_mic_send(
    mut send: SendStream,
    mut mic_rx: mpsc::Receiver<Vec<f32>>,
    cancel: CancellationToken,
) {
    let mut encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            log::error!("[stage-speaker] failed to create mic encoder: {e}");
            cancel.cancel();
            return;
        }
    };

    let mut seq: u32 = 0;
    let mut timestamp: u32 = 0;

    'outer: loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            samples = mic_rx.recv() => {
                let Some(s) = samples else { break };
                for packet in encoder.push_samples(&s) {
                    let write = write_audio_frame(&mut send, seq, timestamp, TAG_NORMAL, &packet);
                    if !matches!(tokio::time::timeout(IO_TIMEOUT, write).await, Ok(Ok(()))) {
                        log::warn!("[stage-speaker] mic send failed, host may have disconnected");
                        cancel.cancel();
                        break 'outer;
                    }
                    seq = seq.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(SAMPLES_PER_FRAME as u32);
                }
            }
        }
    }

    let _ = send.finish();
    log::info!("[stage-speaker] mic send stopped ({seq} frames)");
}

/// Parameters of the speaker audio pipeline.
pub(super) struct SpeakerConfig {
    pub(super) endpoint: Endpoint,
    pub(super) host_node_id: String,
    pub(super) signer: Arc<ControlSigner>,
    pub(super) speaker_mixer: SpeakerMixerHandle,
    pub(super) muted: Arc<AtomicBool>,
    pub(super) cancel: CancellationToken,
}

/// Connect to the host as a speaker (SFU model) with automatic reconnect.
///
/// Each attempt creates its own microphone thread (capture + AEC), started after the
/// host accepted the connection. The `SpeakerMixerHandle` (AudioPlayback) persists across
/// reconnects. The delay returns to `BACKOFF_INITIAL` after a session the host accepted
/// and in which the microphone opened.
pub(super) async fn start_speaker_pipeline(config: SpeakerConfig) {
    let mut backoff = BACKOFF_INITIAL;

    let host_id: EndpointId = match config.host_node_id.parse() {
        Ok(id) => id,
        Err(e) => {
            log::error!("[stage-speaker] invalid host node id: {e}");
            return;
        }
    };

    loop {
        let outcome = run_speaker_once(&config, host_id).await;
        if config.cancel.is_cancelled() {
            break;
        }
        match outcome {
            SessionEnd::Banned => {
                log::info!("[stage-speaker] rejected as banned, stopping");
                break;
            }
            SessionEnd::EndedAfterAudio => backoff = BACKOFF_INITIAL,
            SessionEnd::Failed | SessionEnd::AuthFailed => {}
        }

        log::info!(
            "[stage-speaker] disconnected, reconnecting in {}s",
            backoff.as_secs()
        );
        tokio::select! {
            _ = config.cancel.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }

    log::info!("[stage-speaker] pipeline stopped");
}

/// One speaker connection attempt: connect to the host, wait for its acceptance, start
/// the microphone thread, run until the connection ends or `config.cancel` is cancelled.
/// Returns `EndedAfterAudio` if the host accepted the connection and the microphone
/// opened.
async fn run_speaker_once(config: &SpeakerConfig, host_id: EndpointId) -> SessionEnd {
    let opened = tokio::select! {
        _ = config.cancel.cancelled() => return SessionEnd::Failed,
        opened = open_audio_connection(&config.endpoint, host_id, CONN_TYPE_SPEAKER, &config.signer) => opened,
    };
    let (conn, send, mut recv) = match opened {
        Ok(streams) => streams,
        Err(e) => {
            log::warn!("[stage-speaker] connection to host failed: {e}");
            return SessionEnd::Failed;
        }
    };

    // The microphone stays closed until the host confirmed that it accepts this
    // connection (the local identity has a speaking role and is not banned).
    let mut accepted = [0u8; 1];
    let confirmed = tokio::select! {
        _ = config.cancel.cancelled() => return SessionEnd::Failed,
        confirmed = tokio::time::timeout(IO_TIMEOUT, recv.read_exact(&mut accepted)) => confirmed,
    };
    if !matches!(confirmed, Ok(Ok(()))) || accepted != [SPEAKER_ACCEPTED] {
        let banned = closed_with_code(&conn, CLOSE_BANNED);
        log::warn!("[stage-speaker] host did not accept the speaker connection");
        conn.close(CLOSE_NORMAL.into(), b"not accepted");
        return if banned {
            SessionEnd::Banned
        } else {
            SessionEnd::Failed
        };
    }
    log::info!("[stage-speaker] connected to host");

    // Cancelled on every return path: stops the microphone thread, the mic send task
    // and the stream receive tasks of this session.
    let session_cancel = config.cancel.child_token();
    let _session_guard = session_cancel.clone().drop_guard();

    // Fresh AEC far-end channel for this session.
    let (far_end_tx, far_end_rx) = mpsc::channel::<Vec<f32>>(20);
    config.speaker_mixer.set_far_end_tx(far_end_tx).await;

    let (mic_tx, mic_rx) = mpsc::channel::<Vec<f32>>(PCM_CHANNEL_CAPACITY);
    let started = spawn_mic_thread(MicConfig {
        device: None,
        shared_capture: Arc::new(Mutex::new(None)),
        muted: config.muted.clone(),
        far_end_rx,
        out_tx: mic_tx,
        cancel: session_cancel.clone(),
        label: "[stage-speaker]",
    });
    let mic_error = match started.await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_) => Some("microphone thread did not start".to_string()),
    };
    if let Some(e) = mic_error {
        log::error!("[stage-speaker] mic capture failed: {e}");
        conn.close(CLOSE_NORMAL.into(), b"no microphone");
        return SessionEnd::Failed;
    }

    tokio::spawn(speaker_mic_send(send, mic_rx, session_cancel.clone()));

    let mut next_stream_id: u32 = 0;
    loop {
        tokio::select! {
            _ = session_cancel.cancelled() => break,
            result = conn.accept_uni() => {
                match result {
                    Ok(uni_recv) => {
                        let stream_id = next_stream_id;
                        next_stream_id = next_stream_id.wrapping_add(1);
                        let (stream_tx, stream_rx) = mpsc::channel::<Vec<f32>>(PCM_CHANNEL_CAPACITY);
                        config.speaker_mixer.add_stream(stream_id, stream_rx).await;
                        tokio::spawn(speaker_stream_recv(
                            uni_recv,
                            stream_id,
                            stream_tx,
                            config.speaker_mixer.clone(),
                            session_cancel.child_token(),
                        ));
                    }
                    Err(e) => {
                        log::info!("[stage-speaker] host connection ended: {e}");
                        break;
                    }
                }
            }
        }
    }

    let banned = closed_with_code(&conn, CLOSE_BANNED);
    conn.close(CLOSE_NORMAL.into(), b"speaker left");
    if banned {
        SessionEnd::Banned
    } else {
        SessionEnd::EndedAfterAudio
    }
}
