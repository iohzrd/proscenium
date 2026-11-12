use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer as _, Observer as _, Producer as _, Split as _};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use super::capture::{STREAM_OPEN_TIMEOUT, device_name};
use super::codec::{FRAME_SIZE, SAMPLE_RATE};
use super::device::{DeviceFormat, LinearResampler, choose_format, lock};

/// Ring buffer capacity in frames for a stream decoded as it arrives (calls, the stage
/// host and speakers). A caller cannot buffer more than the capacity it chose.
pub const PLAYBACK_CAPACITY_FRAMES: usize = 16;

/// Counters shared between the producer side and the output callback.
struct PlaybackShared {
    /// Output callbacks that found the ring empty since the last `drain_underruns`.
    underruns: AtomicUsize,
    /// Samples that must be buffered before the callback starts reading the ring,
    /// at stream start and again after each underrun. Zero disables the wait.
    prefill_samples: AtomicUsize,
    /// Ring buffer capacity in frames, kept when the output device is switched.
    capacity_frames: usize,
}

/// Producer handle returned by [`AudioPlayback::start`].
///
/// Push decoded 48 kHz mono samples with [`PlaybackProducer::push`]. If the buffer is
/// full the excess is discarded (the listener is behind; discarding bounds the lag).
pub struct PlaybackProducer {
    inner: Arc<Mutex<ringbuf::HeapProd<f32>>>,
    shared: Arc<PlaybackShared>,
}

impl PlaybackProducer {
    /// Push samples into the jitter buffer. Returns the number actually pushed;
    /// any remainder was dropped because the buffer was full.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        lock(&self.inner).push_slice(samples)
    }

    /// Atomically read and reset the accumulated underrun count since the last
    /// call. Safe to call from any thread.
    pub fn drain_underruns(&self) -> usize {
        self.shared.underruns.swap(0, Ordering::Relaxed)
    }

    /// Set how many 20 ms frames must be buffered before the output callback reads the
    /// ring. The callback writes silence until that depth is reached, at stream start and
    /// again after each underrun.
    pub fn set_prefill_frames(&self, frames: usize) {
        let frames = frames.min(self.shared.capacity_frames - 1);
        self.shared
            .prefill_samples
            .store(frames * FRAME_SIZE, Ordering::Relaxed);
    }
}

/// Audio playback handle. Owns the cpal output stream, whose callback reads the consumer
/// half of the ring buffer. Drop to stop playback. Supports mid-call device switching.
pub struct AudioPlayback {
    stream: cpal::Stream,
    /// Shared producer handle -- allows swapping the ring buffer on device switch.
    producer: Arc<Mutex<ringbuf::HeapProd<f32>>>,
    shared: Arc<PlaybackShared>,
}

/// Find an output device by name, falling back to the default.
fn find_output_device(name: Option<&str>) -> Result<cpal::Device, cpal::Error> {
    let host = cpal::default_host();
    if let Some(name) = name {
        if let Some(device) = host
            .output_devices()
            .ok()
            .and_then(|mut devices| devices.find(|d| device_name(d) == name))
        {
            return Ok(device);
        }
        log::warn!("[audio-playback] device '{name}' not found, using default");
    }
    host.default_output_device()
        .ok_or_else(|| cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable))
}

/// State owned by the output callback: reads 48 kHz mono samples from the ring and
/// converts them to the device rate.
struct RingSource {
    cons: ringbuf::HeapCons<f32>,
    shared: Arc<PlaybackShared>,
    resampler: Option<LinearResampler>,
    /// False while waiting for `prefill_samples` to accumulate.
    primed: bool,
}

impl RingSource {
    /// Fill an interleaved output buffer of `channels` channels. Every channel of a frame
    /// gets the same sample. Writes silence while the ring is below the prefill depth and
    /// for samples the ring cannot supply. Does not allocate or lock.
    fn fill<T>(&mut self, data: &mut [T], channels: usize)
    where
        T: SizedSample + FromSample<f32>,
    {
        let prefill = self.shared.prefill_samples.load(Ordering::Relaxed);
        if !self.primed {
            if self.cons.occupied_len() < prefill {
                data.fill(T::EQUILIBRIUM);
                return;
            }
            self.primed = true;
        }

        let mut underrun = false;
        let cons = &mut self.cons;
        let mut next = || {
            cons.try_pop().unwrap_or_else(|| {
                underrun = true;
                0.0
            })
        };
        for frame in data.chunks_exact_mut(channels.max(1)) {
            let sample = match self.resampler.as_mut() {
                Some(resampler) => resampler.pull(&mut next),
                None => next(),
            };
            frame.fill(T::from_sample(sample));
        }

        if underrun {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            if prefill > 0 {
                self.primed = false;
            }
        }
    }
}

/// Build an output stream of sample type `T` that plays `source`.
fn build_output<T>(
    device: &cpal::Device,
    format: DeviceFormat,
    mut source: RingSource,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(format.config.channels.max(1));
    device.build_output_stream(
        format.config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| source.fill(data, channels),
        |err| log::error!("[audio-playback] stream error: {err}"),
        Some(STREAM_OPEN_TIMEOUT),
    )
}

/// Open the output device with a format it supports and start the stream.
/// Returns the write side of a new ring and the stream.
fn build_playback_stream(
    device_name_pref: Option<&str>,
    shared: &Arc<PlaybackShared>,
) -> Result<(ringbuf::HeapProd<f32>, cpal::Stream), cpal::Error> {
    let device = find_output_device(device_name_pref)?;
    let format = choose_format(
        device.supported_output_configs().into_iter().flatten(),
        device.default_output_config().ok(),
    )
    .unwrap_or_else(DeviceFormat::codec_native);

    log::info!(
        "[audio-playback] device: {} ({} Hz, {} ch, {:?})",
        device_name(&device),
        format.config.sample_rate,
        format.config.channels,
        format.sample_format
    );

    let (prod, cons) = ringbuf::HeapRb::<f32>::new(shared.capacity_frames * FRAME_SIZE).split();
    let source = RingSource {
        cons,
        shared: shared.clone(),
        resampler: LinearResampler::new(SAMPLE_RATE, format.config.sample_rate),
        primed: false,
    };

    let stream = match format.sample_format {
        SampleFormat::F32 => build_output::<f32>(&device, format, source),
        SampleFormat::I16 => build_output::<i16>(&device, format, source),
        SampleFormat::I32 => build_output::<i32>(&device, format, source),
        SampleFormat::U16 => build_output::<u16>(&device, format, source),
        SampleFormat::F64 => build_output::<f64>(&device, format, source),
        SampleFormat::I8 => build_output::<i8>(&device, format, source),
        SampleFormat::U8 => build_output::<u8>(&device, format, source),
        SampleFormat::U32 => build_output::<u32>(&device, format, source),
        _ => Err(cpal::Error::new(cpal::ErrorKind::UnsupportedConfig)),
    }?;
    stream.play()?;

    Ok((prod, stream))
}

impl AudioPlayback {
    /// Start playing audio on the specified device (or default if None), with a ring
    /// buffer of `capacity_frames` frames (at least 2).
    /// Opens the device on the calling thread; call it from a thread that may block.
    pub fn start(
        device_name_pref: Option<&str>,
        capacity_frames: usize,
    ) -> Result<(PlaybackProducer, Self), cpal::Error> {
        let shared = Arc::new(PlaybackShared {
            underruns: AtomicUsize::new(0),
            prefill_samples: AtomicUsize::new(0),
            capacity_frames: capacity_frames.max(2),
        });
        let (prod, stream) = build_playback_stream(device_name_pref, &shared)?;

        let producer = Arc::new(Mutex::new(prod));

        let playback_producer = PlaybackProducer {
            inner: producer.clone(),
            shared: shared.clone(),
        };
        Ok((
            playback_producer,
            Self {
                stream,
                producer,
                shared,
            },
        ))
    }

    /// Open the output device on a blocking thread of the Tokio runtime.
    pub async fn start_blocking(
        device_name_pref: Option<String>,
        capacity_frames: usize,
    ) -> Result<(PlaybackProducer, Self), String> {
        tokio::task::spawn_blocking(move || {
            Self::start(device_name_pref.as_deref(), capacity_frames)
        })
        .await
        .map_err(|e| format!("playback start task failed: {e}"))?
        .map_err(|e| e.to_string())
    }

    /// Switch to a different output device mid-stream. Builds a new stream
    /// and ring buffer, swaps the producer so the decode thread feeds the new
    /// buffer, then drops the old stream. Opens the device on the calling thread.
    pub fn switch_device(&mut self, device_name_pref: Option<&str>) -> Result<(), cpal::Error> {
        let (new_prod, new_stream) = build_playback_stream(device_name_pref, &self.shared)?;
        // Swap the producer under the lock so the decode thread starts
        // pushing into the new ring buffer immediately.
        *lock(&self.producer) = new_prod;
        self.stream = new_stream; // old stream dropped here
        Ok(())
    }
}

/// Shared handle for mid-call device switching from the command layer.
pub type SharedPlayback = Arc<Mutex<Option<AudioPlayback>>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn source(prefill_frames: usize) -> (ringbuf::HeapProd<f32>, RingSource) {
        let shared = Arc::new(PlaybackShared {
            underruns: AtomicUsize::new(0),
            prefill_samples: AtomicUsize::new(prefill_frames * FRAME_SIZE),
            capacity_frames: PLAYBACK_CAPACITY_FRAMES,
        });
        let (prod, cons) =
            ringbuf::HeapRb::<f32>::new(PLAYBACK_CAPACITY_FRAMES * FRAME_SIZE).split();
        (
            prod,
            RingSource {
                cons,
                shared,
                resampler: None,
                primed: false,
            },
        )
    }

    #[test]
    fn output_is_silent_until_prefill_depth_is_buffered() {
        let (mut prod, mut source) = source(3);
        let frame = vec![0.5f32; FRAME_SIZE];
        let mut out = vec![1.0f32; FRAME_SIZE];

        prod.push_slice(&frame);
        prod.push_slice(&frame);
        source.fill(&mut out, 1);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(source.cons.occupied_len(), 2 * FRAME_SIZE);

        prod.push_slice(&frame);
        source.fill(&mut out, 1);
        assert!(out.iter().all(|s| *s == 0.5));
        assert_eq!(source.shared.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn underrun_is_counted_and_restarts_the_prefill_wait() {
        let (mut prod, mut source) = source(2);
        let frame = vec![0.5f32; FRAME_SIZE];
        let mut out = vec![0.0f32; FRAME_SIZE];
        prod.push_slice(&frame);
        prod.push_slice(&frame);
        source.fill(&mut out, 1);
        source.fill(&mut out, 1);
        assert_eq!(source.shared.underruns.load(Ordering::Relaxed), 0);

        // Ring is empty: silence, one underrun, and one frame is not enough to resume.
        source.fill(&mut out, 1);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(source.shared.underruns.load(Ordering::Relaxed), 1);
        prod.push_slice(&frame);
        source.fill(&mut out, 1);
        assert!(out.iter().all(|s| *s == 0.0));
        prod.push_slice(&frame);
        source.fill(&mut out, 1);
        assert!(out.iter().all(|s| *s == 0.5));
    }

    #[test]
    fn mono_sample_is_copied_to_every_channel_and_converted() {
        let (mut prod, mut source) = source(0);
        prod.push_slice(&[1.0, -1.0]);
        let mut out = [0i16; 4];
        source.fill(&mut out, 2);
        assert_eq!(out, [i16::MAX, i16::MAX, i16::MIN, i16::MIN]);
    }
}
