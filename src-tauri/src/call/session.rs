use crate::audio::{
    AudioPlayback, MicConfig, OpusDecoder, OpusEncoder, PCM_CHANNEL_CAPACITY, SAMPLES_PER_FRAME,
    SharedPlayback, TAG_NORMAL, android, lock, read_audio_frame, spawn_mic_thread,
    write_audio_frame,
};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use proscenium_types::{CallState, short_id};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::state::{CallPhase, IO_TIMEOUT, PeerNotice};

/// A call whose peer sends no audio frame for this long has failed.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

impl super::CallHandler {
    /// Run the bidirectional audio session of the active call `call_id` on an
    /// authenticated stream. Both the caller and the callee use this function.
    ///
    /// Requires the call to be in phase `Active`. Returns when the call ends, either
    /// stream ends, or the microphone cannot be opened. On return the session token is
    /// cancelled (which stops the microphone thread and both stream tasks), the
    /// connection is closed, and the call, if it is still registered, is ended and the
    /// peer is sent `CallHangup`.
    pub(super) async fn run_audio_session(
        &self,
        conn: Connection,
        send: SendStream,
        recv: RecvStream,
        call_id: &str,
    ) {
        let session = {
            let lock = self.active_call.lock().await;
            match lock.as_ref() {
                Some(call) if call.call_id == call_id && call.phase == CallPhase::Active => Some((
                    call.cancel.child_token(),
                    call.muted.clone(),
                    call.capture.clone(),
                    call.playback.clone(),
                )),
                _ => None,
            }
        };
        let Some((session_cancel, muted, shared_capture, shared_playback)) = session else {
            conn.close(0u32.into(), b"call ended");
            return;
        };
        // Cancelled on every return path.
        let _session_guard = session_cancel.clone().drop_guard();

        let (input_dev, output_dev) = self.audio_device_prefs().await;

        // On Android, set MODE_IN_COMMUNICATION before opening audio streams
        android::enter_communication_mode();

        // mic thread (capture -> AEC -> mute) -> mic_rx -> send loop
        // recv loop -> far_end_tx -> mic thread (AEC reference)
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<f32>>(PCM_CHANNEL_CAPACITY);
        let (far_end_tx, far_end_rx) = mpsc::channel::<Vec<f32>>(20);
        let mic_started = spawn_mic_thread(MicConfig {
            device: input_dev,
            shared_capture,
            muted,
            far_end_rx,
            out_tx: mic_tx,
            cancel: session_cancel.clone(),
            label: "[call]",
        });
        let mic_error = match mic_started.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(_) => Some("microphone thread did not start".to_string()),
        };
        if let Some(e) = mic_error {
            log::error!("[call] failed to start audio capture: {e}");
            conn.close(0u32.into(), b"no microphone");
            self.end_call(call_id, CallState::Failed, PeerNotice::Hangup)
                .await;
            return;
        }

        let send_task = tokio::spawn(send_audio_loop(send, session_cancel.clone(), mic_rx));
        let recv_task = tokio::spawn(recv_audio_loop(
            recv,
            session_cancel.clone(),
            output_dev,
            shared_playback,
            far_end_tx,
        ));

        tokio::select! {
            _ = session_cancel.cancelled() => {
                log::info!("[call] session cancelled for {}", short_id(call_id));
            }
            result = send_task => {
                if let Err(e) = result {
                    log::error!("[call] send task failed: {e}");
                }
            }
            result = recv_task => {
                if let Err(e) = result {
                    log::error!("[call] recv task failed: {e}");
                }
            }
        }

        session_cancel.cancel();
        conn.close(0u32.into(), b"call ended");
        log::info!("[call] audio session ended for {}", short_id(call_id));

        // If the call is still registered, the peer was not told that it ended.
        self.end_call(call_id, CallState::Ended, PeerNotice::Hangup)
            .await;
    }
}

/// Read AEC-cleaned samples, encode to Opus, send over QUIC.
async fn send_audio_loop(
    mut send: SendStream,
    cancel: CancellationToken,
    mut rx: mpsc::Receiver<Vec<f32>>,
) {
    let mut encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            log::error!("[call] failed to create Opus encoder: {e}");
            return;
        }
    };

    let mut seq: u32 = 0;
    let mut timestamp: u32 = 0;

    'outer: loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            samples = rx.recv() => {
                let Some(samples) = samples else { break };
                for packet in encoder.push_samples(&samples) {
                    let write = write_audio_frame(&mut send, seq, timestamp, TAG_NORMAL, &packet);
                    match tokio::time::timeout(IO_TIMEOUT, write).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            log::info!("[call] send error: {e}");
                            break 'outer;
                        }
                        Err(_) => {
                            log::warn!("[call] send stalled for {IO_TIMEOUT:?}");
                            break 'outer;
                        }
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

/// Receive Opus frames from QUIC, decode, play back, and feed AEC far-end.
async fn recv_audio_loop(
    mut recv: RecvStream,
    cancel: CancellationToken,
    output_device: Option<String>,
    shared_playback: SharedPlayback,
    far_end_tx: mpsc::Sender<Vec<f32>>,
) {
    let mut decoder = match OpusDecoder::new() {
        Ok(d) => d,
        Err(e) => {
            log::error!("[call] failed to create Opus decoder: {e}");
            return;
        }
    };

    let (mut prod, playback) = match AudioPlayback::start_blocking(output_device).await {
        Ok(p) => p,
        Err(e) => {
            log::error!("[call] failed to start audio playback: {e}");
            return;
        }
    };

    // Store in shared handle so switch_output_device can reach it
    *lock(&shared_playback) = Some(playback);

    let mut frames_received: u32 = 0;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = tokio::time::timeout(STREAM_IDLE_TIMEOUT, read_audio_frame(&mut recv)) => {
                match result {
                    Ok(Ok(Some(frame))) => {
                        if let Ok(samples) = decoder.decode(&frame.payload) {
                            // Feed far-end reference to AEC before playback
                            let _ = far_end_tx.try_send(samples.clone());
                            prod.push(&samples);
                        }
                        frames_received = frames_received.wrapping_add(1);
                    }
                    Ok(Ok(None)) => {
                        log::info!("[call] remote stream closed");
                        break;
                    }
                    Ok(Err(e)) => {
                        log::info!("[call] recv error: {e}");
                        break;
                    }
                    Err(_) => {
                        log::warn!("[call] no audio frame within {STREAM_IDLE_TIMEOUT:?}");
                        break;
                    }
                }
            }
        }
    }

    // Clear the shared handle; this closes the output device.
    lock(&shared_playback).take();

    log::info!("[call] recv loop ended ({frames_received} frames received)");
}
