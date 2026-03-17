use crate::audio::{read_audio_frame, write_audio_frame};
use crate::error::AppError;
use iroh::endpoint::{RecvStream, SendStream};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

#[allow(dead_code)]
/// A single Opus audio frame forwarded to all relay subscribers.
#[derive(Clone, Debug)]
pub struct RelayFrame {
    pub seq: u32,
    pub timestamp: u32,
    /// Wire tag byte (TAG_NORMAL or TAG_CHECKPOINT) — preserved for auth forwarding.
    pub tag: u8,
    pub payload: Arc<Vec<u8>>,
}

// ---- Command enum -------------------------------------------------------

#[allow(dead_code)]
pub enum RelayCommand {
    /// New downstream listener connected — add their send stream to the relay fanout.
    AddDownstream(SendStream),
    Shutdown,
}

// ---- Handle -------------------------------------------------------------

/// Cheap-to-clone handle to the RelayActor.
#[derive(Clone)]
pub struct RelayHandle {
    cmd_tx: mpsc::Sender<RelayCommand>,
}

#[allow(dead_code)]
impl RelayHandle {
    pub async fn add_downstream(&self, stream: SendStream) -> Result<(), AppError> {
        self.cmd_tx
            .send(RelayCommand::AddDownstream(stream))
            .await
            .map_err(|_| AppError::Other("relay actor closed".into()))
    }
}

// ---- Actor --------------------------------------------------------------

#[allow(dead_code)]
/// Spawn the RelayActor.
///
/// The relay reads frames from `upstream` (a `RecvStream` from the host or
/// an upstream relay) and fans them out to all downstream subscribers.
/// Call `handle.add_downstream(send_stream)` whenever a new listener connects.
pub fn spawn_relay(
    upstream: RecvStream,
    cancel: CancellationToken,
) -> Result<RelayHandle, AppError> {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    tokio::spawn(run_relay(upstream, cmd_rx, cancel));
    Ok(RelayHandle { cmd_tx })
}

async fn run_relay(
    mut upstream: RecvStream,
    mut cmd_rx: mpsc::Receiver<RelayCommand>,
    cancel: CancellationToken,
) {
    // Relay-local broadcast channel: same 50-frame buffer, laggards skip.
    let (frame_tx, _) = broadcast::channel::<RelayFrame>(50);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(RelayCommand::AddDownstream(stream)) => {
                        let mut rx = frame_tx.subscribe();
                        let sub_cancel = cancel.child_token();
                        tokio::spawn(async move {
                            run_downstream_subscriber(stream, &mut rx, sub_cancel).await;
                        });
                    }
                    Some(RelayCommand::Shutdown) | None => break,
                }
            }

            result = read_audio_frame(&mut upstream) => {
                match result {
                    Ok(Some((seq, timestamp, tag, payload))) => {
                        let frame = RelayFrame {
                            seq,
                            timestamp,
                            tag,
                            payload: Arc::new(payload),
                        };
                        // Non-blocking send — lagging subscribers are skipped
                        let _ = frame_tx.send(frame);
                    }
                    Ok(None) => {
                        log::info!("[relay] upstream stream ended");
                        break;
                    }
                    Err(e) => {
                        log::warn!("[relay] upstream read error: {e}");
                        break;
                    }
                }
            }
        }
    }

    log::info!("[relay] actor stopped");
}

async fn run_downstream_subscriber(
    mut stream: SendStream,
    rx: &mut broadcast::Receiver<RelayFrame>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = rx.recv() => {
                match result {
                    Ok(frame) => {
                        if write_audio_frame(
                            &mut stream,
                            frame.seq,
                            frame.timestamp,
                            frame.tag,
                            &frame.payload,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::debug!("[relay] subscriber lagged {n} frames — skipping");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    let _ = stream.finish();
}
