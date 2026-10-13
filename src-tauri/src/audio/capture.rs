use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer as _, Producer as _, Split as _};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::codec::{FRAME_SIZE, SAMPLE_RATE};
use super::device::{DeviceFormat, LinearResampler, choose_format, lock};

/// Capture ring capacity: 10 frames (200 ms) of 48 kHz mono samples. The input callback
/// discards samples that do not fit.
const CAPTURE_CAPACITY: usize = FRAME_SIZE * 10;

/// Time the backend may take to open a stream.
pub(super) const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle of the thread blocked in [`CaptureConsumer::read`]. The input callback
/// unparks it after writing samples.
struct ReaderWake {
    reader: OnceLock<std::thread::Thread>,
}

impl ReaderWake {
    fn notify(&self) {
        if let Some(thread) = self.reader.get() {
            thread.unpark();
        }
    }
}

/// Read side of the capture ring: 48 kHz mono f32 samples.
/// All calls must come from one thread.
pub struct CaptureConsumer {
    cons: Arc<Mutex<ringbuf::HeapCons<f32>>>,
    wake: Arc<ReaderWake>,
}

impl CaptureConsumer {
    /// Copy captured samples into `out` and return how many were copied.
    /// Blocks the calling thread for at most `timeout` while no sample is available.
    pub fn read(&mut self, out: &mut [f32], timeout: Duration) -> usize {
        self.wake.reader.get_or_init(std::thread::current);
        let copied = lock(&self.cons).pop_slice(out);
        if copied > 0 {
            return copied;
        }
        std::thread::park_timeout(timeout);
        lock(&self.cons).pop_slice(out)
    }
}

/// Audio capture handle. Owns the cpal input stream, whose callback converts the device
/// format to 48 kHz mono f32 and writes into a preallocated ring. Drop to stop capture.
/// Supports mid-call device switching via [`AudioCapture::switch_device`].
pub struct AudioCapture {
    stream: cpal::Stream,
    /// Shared with the [`CaptureConsumer`] so a device switch can replace the ring.
    cons: Arc<Mutex<ringbuf::HeapCons<f32>>>,
    wake: Arc<ReaderWake>,
}

pub fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Find an input device by name, falling back to the default.
fn find_input_device(name: Option<&str>) -> Result<cpal::Device, cpal::Error> {
    let host = cpal::default_host();
    if let Some(name) = name {
        if let Some(device) = host
            .input_devices()
            .ok()
            .and_then(|mut devices| devices.find(|d| device_name(d) == name))
        {
            return Ok(device);
        }
        log::warn!("[audio-capture] device '{name}' not found, using default");
    }
    host.default_input_device()
        .ok_or_else(|| cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable))
}

/// Build an input stream of sample type `T`. The callback averages the channels of each
/// frame, converts the rate to 48 kHz and writes into `prod` without allocating.
fn build_input<T>(
    device: &cpal::Device,
    format: DeviceFormat,
    mut prod: ringbuf::HeapProd<f32>,
    wake: Arc<ReaderWake>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(format.config.channels.max(1));
    let mut resampler = LinearResampler::new(format.config.sample_rate, SAMPLE_RATE);
    device.build_input_stream(
        format.config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            for frame in data.chunks_exact(channels) {
                let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
                let mono = sum / channels as f32;
                match resampler.as_mut() {
                    Some(resampler) => resampler.push(mono, |s| {
                        let _ = prod.try_push(s);
                    }),
                    None => {
                        let _ = prod.try_push(mono);
                    }
                }
            }
            wake.notify();
        },
        |err| log::error!("[audio-capture] stream error: {err}"),
        Some(STREAM_OPEN_TIMEOUT),
    )
}

/// Open the input device with a format it supports and start the stream.
/// Returns the stream and the read side of a new ring.
fn build_capture_stream(
    device_name_pref: Option<&str>,
    wake: &Arc<ReaderWake>,
) -> Result<(cpal::Stream, ringbuf::HeapCons<f32>), cpal::Error> {
    let device = find_input_device(device_name_pref)?;
    let format = choose_format(
        device.supported_input_configs().into_iter().flatten(),
        device.default_input_config().ok(),
    )
    .unwrap_or_else(DeviceFormat::codec_native);

    log::info!(
        "[audio-capture] device: {} ({} Hz, {} ch, {:?})",
        device_name(&device),
        format.config.sample_rate,
        format.config.channels,
        format.sample_format
    );

    let (prod, cons) = ringbuf::HeapRb::<f32>::new(CAPTURE_CAPACITY).split();
    let wake = wake.clone();
    let stream = match format.sample_format {
        SampleFormat::F32 => build_input::<f32>(&device, format, prod, wake),
        SampleFormat::I16 => build_input::<i16>(&device, format, prod, wake),
        SampleFormat::I32 => build_input::<i32>(&device, format, prod, wake),
        SampleFormat::U16 => build_input::<u16>(&device, format, prod, wake),
        SampleFormat::F64 => build_input::<f64>(&device, format, prod, wake),
        SampleFormat::I8 => build_input::<i8>(&device, format, prod, wake),
        SampleFormat::U8 => build_input::<u8>(&device, format, prod, wake),
        SampleFormat::U32 => build_input::<u32>(&device, format, prod, wake),
        _ => Err(cpal::Error::new(cpal::ErrorKind::UnsupportedConfig)),
    }?;
    stream.play()?;
    Ok((stream, cons))
}

impl AudioCapture {
    /// Start capturing audio from the specified device (or default if None).
    /// Opens the device on the calling thread; call it from a thread that may block.
    pub fn start(device_name_pref: Option<&str>) -> Result<(Self, CaptureConsumer), cpal::Error> {
        let wake = Arc::new(ReaderWake {
            reader: OnceLock::new(),
        });
        let (stream, cons) = build_capture_stream(device_name_pref, &wake)?;
        let cons = Arc::new(Mutex::new(cons));
        let consumer = CaptureConsumer {
            cons: cons.clone(),
            wake: wake.clone(),
        };
        Ok((Self { stream, cons, wake }, consumer))
    }

    /// Switch to a different input device mid-stream. Builds the new stream and ring
    /// first, then replaces the ring read by the [`CaptureConsumer`] and drops the old
    /// stream. Opens the device on the calling thread.
    pub fn switch_device(&mut self, device_name_pref: Option<&str>) -> Result<(), cpal::Error> {
        let (stream, cons) = build_capture_stream(device_name_pref, &self.wake)?;
        *lock(&self.cons) = cons;
        self.stream = stream; // old stream dropped here
        Ok(())
    }
}

/// Shared handle for mid-call device switching from the command layer.
pub type SharedCapture = Arc<Mutex<Option<AudioCapture>>>;
