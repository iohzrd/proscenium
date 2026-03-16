use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::codec::{CHANNELS, SAMPLE_RATE};

/// Audio capture handle. Reads from the default input device and sends
/// f32 sample chunks to the provided channel.
pub struct AudioCapture {
    _stream: cpal::Stream,
}

/// Audio playback handle. Reads f32 samples from the provided channel
/// and writes them to the default output device.
pub struct AudioPlayback {
    _stream: cpal::Stream,
}

/// Build the cpal stream config for our Opus parameters.
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

impl AudioCapture {
    /// Start capturing audio from the default input device.
    /// Sends chunks of f32 samples (may not be frame-aligned; the encoder handles buffering).
    pub fn start(tx: mpsc::Sender<Vec<f32>>) -> Result<Self, cpal::BuildStreamError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .expect("no default input device");
        let config = stream_config();

        log::info!("[audio-capture] device: {}", device_name(&device));

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Non-blocking send: drop samples if receiver is too slow
                let _ = tx.try_send(data.to_vec());
            },
            |err| log::error!("[audio-capture] stream error: {err}"),
            None,
        )?;
        stream.play().expect("failed to start capture stream");
        Ok(Self { _stream: stream })
    }
}

impl AudioPlayback {
    /// Start playing audio on the default output device.
    /// Reads f32 samples from the ring buffer and writes to the device.
    pub fn start(rx: Arc<std::sync::Mutex<Vec<f32>>>) -> Result<Self, cpal::BuildStreamError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .expect("no default output device");
        let config = stream_config();

        log::info!("[audio-playback] device: {}", device_name(&device));

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut buf = rx.lock().unwrap();
                let available = buf.len().min(data.len());
                data[..available].copy_from_slice(&buf[..available]);
                // Fill remainder with silence
                for sample in &mut data[available..] {
                    *sample = 0.0;
                }
                buf.drain(..available);
            },
            |err| log::error!("[audio-playback] stream error: {err}"),
            None,
        )?;
        stream.play().expect("failed to start playback stream");
        Ok(Self { _stream: stream })
    }
}
