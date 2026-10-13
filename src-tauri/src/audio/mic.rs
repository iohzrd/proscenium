//! Microphone thread shared by calls and stages: capture, echo cancellation, mute.

use super::aec::EchoCanceller;
use super::capture::{AudioCapture, SharedCapture};
use super::device::lock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Samples read from the capture ring per iteration (10 ms at 48 kHz).
const READ_CHUNK: usize = 480;

/// Longest wait for captured samples before the thread checks `cancel` again.
const READ_TIMEOUT: Duration = Duration::from_millis(20);

/// Capacity of the channels that carry microphone and far-end PCM chunks.
pub const PCM_CHANNEL_CAPACITY: usize = 32;

pub struct MicConfig {
    /// Input device name, or `None` for the default device.
    pub device: Option<String>,
    /// Receives the open capture so the command layer can switch the device.
    /// Cleared when the thread exits.
    pub shared_capture: SharedCapture,
    /// While set, the thread outputs zeros instead of the microphone signal.
    pub muted: Arc<AtomicBool>,
    /// Played-back audio, used as the echo reference.
    pub far_end_rx: mpsc::Receiver<Vec<f32>>,
    /// Echo-cancelled microphone chunks. A chunk is discarded when the channel is full.
    pub out_tx: mpsc::Sender<Vec<f32>>,
    /// Stops the thread and closes the input device.
    pub cancel: CancellationToken,
    /// Log prefix.
    pub label: &'static str,
}

/// Spawn a thread that opens the input device, removes the echo of `far_end_rx` from the
/// captured signal and sends the result to `out_tx`. The thread exits when `cancel` is
/// cancelled or `out_tx` is closed.
///
/// The returned receiver yields the result of opening the device.
pub fn spawn_mic_thread(config: MicConfig) -> oneshot::Receiver<Result<(), String>> {
    let (started_tx, started_rx) = oneshot::channel();
    let label = config.label;
    let spawned = std::thread::Builder::new()
        .name("mic".to_string())
        .spawn(move || run_mic_thread(config, started_tx));
    if let Err(e) = spawned {
        log::error!("{label} failed to spawn microphone thread: {e}");
    }
    started_rx
}

fn run_mic_thread(mut config: MicConfig, started_tx: oneshot::Sender<Result<(), String>>) {
    let label = config.label;
    let mut consumer = match AudioCapture::start(config.device.as_deref()) {
        Ok((capture, consumer)) => {
            *lock(&config.shared_capture) = Some(capture);
            let _ = started_tx.send(Ok(()));
            consumer
        }
        Err(e) => {
            let _ = started_tx.send(Err(e.to_string()));
            return;
        }
    };

    let mut aec = EchoCanceller::new();
    let mut chunk = [0.0f32; READ_CHUNK];
    log::info!("{label} microphone thread started");

    while !config.cancel.is_cancelled() {
        let read = consumer.read(&mut chunk, READ_TIMEOUT);
        let Some(captured) = chunk.get(..read).filter(|c| !c.is_empty()) else {
            continue;
        };
        while let Ok(far) = config.far_end_rx.try_recv() {
            aec.render(&far);
        }
        let mut cleaned = aec.process_capture(captured);
        if cleaned.is_empty() {
            continue;
        }
        if config.muted.load(Ordering::Relaxed) {
            cleaned.fill(0.0);
        }
        match config.out_tx.try_send(cleaned) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }
    }

    // Dropping the capture closes the input device.
    *lock(&config.shared_capture) = None;
    log::info!("{label} microphone thread ended");
}
