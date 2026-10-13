use crate::audio::write_audio_frame;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A subscriber whose stream accepts no frame for this long is removed.
const SUBSCRIBER_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// A single Opus audio frame forwarded to all subscribers.
#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub seq: u32,
    pub timestamp: u32,
    /// Wire tag byte (TAG_NORMAL or TAG_CHECKPOINT).
    pub tag: u8,
    /// Wire payload bytes (may include checkpoint prefix for TAG_CHECKPOINT frames).
    /// Wrapped in Arc to avoid N copies for N subscribers.
    pub payload: Arc<Vec<u8>>,
}

/// Fan-out: distributes one audio stream to its subscribers via `tokio::sync::broadcast`.
///
/// Not an actor: `broadcast::Sender::send` neither blocks nor locks across an await.
/// Each subscriber gets its own task that writes frames to a QUIC send stream.
pub struct Fanout {
    tx: broadcast::Sender<AudioFrame>,
}

impl Default for Fanout {
    fn default() -> Self {
        Self::new()
    }
}

impl Fanout {
    /// Create a new Fanout with a 50-frame buffer (~1 second of audio at 20ms/frame).
    /// A subscriber that falls more than 50 frames behind skips to the oldest buffered
    /// frame; its receiver sees a gap in the frame sequence.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(50);
        Self { tx }
    }

    /// Send a frame to all active subscribers. Returns at once; a frame sent while
    /// there is no subscriber is discarded.
    pub fn send_frame(&self, seq: u32, timestamp: u32, tag: u8, payload: Vec<u8>) {
        let frame = AudioFrame {
            seq,
            timestamp,
            tag,
            payload: Arc::new(payload),
        };
        // SendError means there is no subscriber, which is normal at start/end of room.
        let _ = self.tx.send(frame);
    }

    /// Subscribe a downstream QUIC send stream to this fanout.
    ///
    /// Spawns a task that writes frames to `stream` until a write fails or takes longer
    /// than `SUBSCRIBER_WRITE_TIMEOUT`, `cancel` is cancelled, or the fanout is dropped.
    /// The returned handle completes when that task ends.
    pub fn add_subscriber(
        &self,
        mut stream: iroh::endpoint::SendStream,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        let mut rx = self.tx.subscribe();

        tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = rx.recv() => match result {
                        Ok(frame) => frame,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            log::debug!("[fanout] subscriber lagged, skipped {n} frames");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                };
                let write = write_audio_frame(
                    &mut stream,
                    frame.seq,
                    frame.timestamp,
                    frame.tag,
                    &frame.payload,
                );
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = tokio::time::timeout(SUBSCRIBER_WRITE_TIMEOUT, write) => {
                        if !matches!(result, Ok(Ok(()))) {
                            break; // stream closed or stalled
                        }
                    }
                }
            }
            let _ = stream.finish();
        })
    }
}
