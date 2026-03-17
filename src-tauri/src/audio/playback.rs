use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer as _, Producer as _, Split as _};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::codec::{CHANNELS, FRAME_SIZE, SAMPLE_RATE};

/// Ring buffer capacity in frames. Callers cannot exceed this depth.
pub const PLAYBACK_CAPACITY_FRAMES: usize = 10;

/// Producer handle returned by [`AudioPlayback::start`].
///
/// Push decoded PCM samples with [`PlaybackProducer::push`]. If the buffer is
/// full the excess is silently dropped (listener is behind; dropping is
/// preferable to unbounded lag).
///
/// Read underrun feedback with [`PlaybackProducer::drain_underruns`] to drive
/// adaptive pre-fill depth in the caller.
pub struct PlaybackProducer {
    inner: ringbuf::HeapProd<f32>,
    underruns: Arc<AtomicUsize>,
}

impl PlaybackProducer {
    /// Push samples into the jitter buffer. Returns the number actually pushed;
    /// any remainder was dropped because the buffer was full.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        self.inner.push_slice(samples)
    }

    /// Atomically read and reset the accumulated underrun count since the last
    /// call. Safe to call from any thread.
    pub fn drain_underruns(&self) -> usize {
        self.underruns.swap(0, Ordering::Relaxed)
    }
}

/// Audio playback handle. Owns the cpal output stream and the consumer half of
/// the ring buffer. Drop to stop playback.
pub struct AudioPlayback {
    _stream: cpal::Stream,
}

fn stream_config() -> StreamConfig {
    let channel_count = match CHANNELS {
        opus::Channels::Mono => 1,
        opus::Channels::Stereo => 2,
    };
    StreamConfig {
        channels: channel_count,
        sample_rate: SAMPLE_RATE,
        buffer_size: cpal::BufferSize::Default,
    }
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

impl AudioPlayback {
    /// Start playing audio on the default output device.
    ///
    /// Creates a lock-free ring buffer with [`PLAYBACK_CAPACITY_FRAMES`] frames
    /// of capacity, starts the cpal stream, and returns a [`PlaybackProducer`]
    /// for pushing PCM samples. The cpal callback is fully lock-free: it calls
    /// `pop_slice` and zero-fills any underrun, incrementing the counter on the
    /// producer handle.
    pub fn start() -> Result<(PlaybackProducer, Self), cpal::BuildStreamError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(cpal::BuildStreamError::DeviceNotAvailable)?;
        let config = stream_config();

        log::info!("[audio-playback] device: {}", device_name(&device));

        let ring = ringbuf::HeapRb::<f32>::new(PLAYBACK_CAPACITY_FRAMES * FRAME_SIZE);
        let (prod, mut cons) = ring.split();

        let underruns = Arc::new(AtomicUsize::new(0));
        let underruns_cb = underruns.clone();

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let available = cons.pop_slice(data);
                if available < data.len() {
                    underruns_cb.fetch_add(1, Ordering::Relaxed);
                    data[available..].fill(0.0);
                }
            },
            |err| log::error!("[audio-playback] stream error: {err}"),
            None,
        )?;
        stream
            .play()
            .map_err(|e| cpal::BuildStreamError::BackendSpecific {
                err: cpal::BackendSpecificError {
                    description: e.to_string(),
                },
            })?;

        let producer = PlaybackProducer {
            inner: prod,
            underruns,
        };
        Ok((producer, Self { _stream: stream }))
    }
}
