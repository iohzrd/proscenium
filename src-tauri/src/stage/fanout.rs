use crate::audio::write_audio_frame;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

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

/// Fan-out: distributes a mixed audio stream to an arbitrary number of
/// downstream listeners via `tokio::sync::broadcast`.
///
/// Not an actor — `broadcast::Sender` is already non-blocking and lock-free.
/// Each subscriber gets its own task that writes frames to a QUIC send stream.
pub struct Fanout {
    tx: broadcast::Sender<AudioFrame>,
}

impl Fanout {
    /// Create a new Fanout with a 50-frame buffer (~1 second of audio at 20ms/frame).
    /// Lagging subscribers skip to the latest frame, which is correct for
    /// real-time audio (old frames are useless).
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(50);
        Self { tx }
    }

    /// Send an authenticated frame to all active subscribers.
    /// Non-blocking — never fails even if there are no subscribers or a
    /// subscriber's buffer is full (laggards are silently dropped).
    pub fn send_frame(&self, seq: u32, timestamp: u32, tag: u8, payload: Vec<u8>) {
        let frame = AudioFrame {
            seq,
            timestamp,
            tag,
            payload: Arc::new(payload),
        };
        // Ignore SendError (no subscribers) — normal at start/end of room.
        let _ = self.tx.send(frame);
    }

    /// Subscribe a downstream QUIC send stream to this fanout.
    ///
    /// Spawns a task that writes frames to `stream` until the stream closes,
    /// the returned token is cancelled (evict one subscriber), or `stage_cancel`
    /// fires (shut down all subscribers with the stage). Always pass the stage's
    /// `CancellationToken` as `stage_cancel` so all subscribers are cleaned up
    /// together on stage teardown.
    pub fn add_subscriber(
        &self,
        mut stream: iroh::endpoint::SendStream,
        stage_cancel: &CancellationToken,
    ) -> CancellationToken {
        let mut rx = self.tx.subscribe();
        let cancel = stage_cancel.child_token();
        let token = cancel.clone();

        tokio::spawn(async move {
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
                                    break; // stream dead
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                log::debug!("[fanout] subscriber lagged {n} frames — skipping");
                                // Skip to latest — correct for real-time audio
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
            let _ = stream.finish();
        });

        token
    }

    /// Subscribe an in-process consumer (no QUIC stream needed).
    ///
    /// Used by the host to hear the mixed output locally without a network
    /// round-trip.
    pub fn subscribe_local(&self) -> broadcast::Receiver<AudioFrame> {
        self.tx.subscribe()
    }

    /// Number of currently active subscriber tasks. Useful for load metrics.
    #[allow(dead_code)]
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}
