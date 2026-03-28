pub mod aec;
pub mod android;
pub mod capture;
pub mod codec;
pub mod playback;
pub mod transport;

pub use aec::EchoCanceller;
pub use capture::{AudioCapture, SharedCapture};
pub use codec::{FRAME_SIZE, OpusDecoder, OpusEncoder, SAMPLES_PER_FRAME};
pub use playback::{AudioPlayback, SharedPlayback};
pub use transport::{TAG_NORMAL, read_audio_frame, write_audio_frame};

use capture::device_name;
use cpal::traits::HostTrait;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
}

/// List available audio input devices (deduplicated by name).
pub fn list_input_devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().map(|d| device_name(&d));
    let mut seen = HashSet::new();
    host.input_devices()
        .map(|devices| {
            devices
                .filter_map(|d| {
                    let name = device_name(&d);
                    if seen.insert(name.clone()) {
                        Some(AudioDevice {
                            is_default: default_name.as_ref() == Some(&name),
                            name,
                        })
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// List available audio output devices (deduplicated by name).
pub fn list_output_devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().map(|d| device_name(&d));
    let mut seen = HashSet::new();
    host.output_devices()
        .map(|devices| {
            devices
                .filter_map(|d| {
                    let name = device_name(&d);
                    if seen.insert(name.clone()) {
                        Some(AudioDevice {
                            is_default: default_name.as_ref() == Some(&name),
                            name,
                        })
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}
