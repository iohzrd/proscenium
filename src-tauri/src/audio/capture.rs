use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;

use super::codec::{CHANNELS, SAMPLE_RATE};

/// Audio capture handle. Reads from the default input device and sends
/// f32 sample chunks to the provided channel.
pub struct AudioCapture {
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

impl AudioCapture {
    /// Start capturing audio from the default input device.
    /// Sends chunks of f32 samples (may not be frame-aligned; the encoder handles buffering).
    pub fn start(tx: mpsc::Sender<Vec<f32>>) -> Result<Self, cpal::BuildStreamError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(cpal::BuildStreamError::DeviceNotAvailable)?;
        let config = stream_config();

        log::info!("[audio-capture] device: {}", device_name(&device));

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.try_send(data.to_vec());
            },
            |err| log::error!("[audio-capture] stream error: {err}"),
            None,
        )?;
        stream
            .play()
            .map_err(|e| cpal::BuildStreamError::BackendSpecific {
                err: cpal::BackendSpecificError {
                    description: e.to_string(),
                },
            })?;
        Ok(Self { _stream: stream })
    }
}
