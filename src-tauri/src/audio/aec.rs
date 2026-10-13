use aec3::api::EchoCanceller3Config;
use aec3::api::control::EchoControl;
use aec3::audio_processing::aec3::echo_canceller3::EchoCanceller3;
use aec3::audio_processing::audio_buffer::AudioBuffer;
use aec3::audio_processing::stream_config::StreamConfig;

/// Number of samples per 10 ms frame at 48 kHz (mono).
const FRAME_SAMPLES: usize = 480;

/// Acoustic echo canceller wrapping `aec3::EchoCanceller3`.
///
/// Operates at 48 kHz mono. The inner `AudioBuffer` requires exactly
/// 480-sample frames, so incoming arbitrary-size chunks are accumulated
/// in `render_buf`/`capture_buf` and drained in frame-sized increments.
///
/// At 48 kHz `EchoCanceller3` reads and writes the three frequency bands of the
/// `AudioBuffer`, so each frame is split into bands before it is passed in and the
/// capture frame is merged back afterwards.
///
/// Usage:
/// 1. Before processing each mic chunk, call `render()` with any pending
///    playback samples to keep the far-end reference up to date.
/// 2. Call `process_capture()` with the raw mic samples; returns cleaned samples.
pub struct EchoCanceller {
    inner: EchoCanceller3,
    /// Accumulates far-end samples until a full frame is available.
    render_buf: Vec<f32>,
    /// Accumulates near-end samples until a full frame is available.
    capture_buf: Vec<f32>,
    /// Reusable 480-sample buffer for feeding render data to AEC3.
    render_audio: AudioBuffer,
    /// Reusable 480-sample buffer for feeding capture data to AEC3.
    capture_audio: AudioBuffer,
    stream_config: StreamConfig,
}

impl EchoCanceller {
    pub fn new() -> Self {
        let config = EchoCanceller3Config::default();
        let inner = EchoCanceller3::new(config, 48_000, 1, 1);
        let stream_config = StreamConfig::new(48_000, 1, false);
        let render_audio = AudioBuffer::new(FRAME_SAMPLES, 1, FRAME_SAMPLES, 1, FRAME_SAMPLES);
        let capture_audio = AudioBuffer::new(FRAME_SAMPLES, 1, FRAME_SAMPLES, 1, FRAME_SAMPLES);
        Self {
            inner,
            render_buf: Vec::new(),
            capture_buf: Vec::new(),
            render_audio,
            capture_audio,
            stream_config,
        }
    }

    /// Feed playback (far-end reference) samples into the AEC render path.
    /// Call this before `process_capture` for the same time slice.
    pub fn render(&mut self, samples: &[f32]) {
        self.render_buf.extend_from_slice(samples);
        let mut offset = 0;
        while let Some(frame) = self.render_buf.get(offset..offset + FRAME_SAMPLES) {
            self.render_audio.copy_from(&[frame], &self.stream_config);
            self.render_audio.split_into_frequency_bands();
            self.inner.analyze_render(&mut self.render_audio);
            offset += FRAME_SAMPLES;
        }
        self.render_buf.drain(..offset);
    }

    /// Process mic samples through AEC. Returns echo-cancelled samples.
    /// Drain all pending render samples first by calling `render()` before this.
    pub fn process_capture(&mut self, samples: &[f32]) -> Vec<f32> {
        self.capture_buf.extend_from_slice(samples);
        let mut out = Vec::with_capacity(self.capture_buf.len());
        let mut cleaned = [0.0f32; FRAME_SAMPLES];
        let mut offset = 0;
        while let Some(frame) = self.capture_buf.get(offset..offset + FRAME_SAMPLES) {
            self.capture_audio.copy_from(&[frame], &self.stream_config);
            self.inner.analyze_capture(&mut self.capture_audio);
            self.capture_audio.split_into_frequency_bands();
            self.inner.process_capture(&mut self.capture_audio, false);
            self.capture_audio.merge_frequency_bands();
            self.capture_audio
                .copy_to_stream(&self.stream_config, &mut [cleaned.as_mut_slice()]);
            out.extend_from_slice(&cleaned);
            offset += FRAME_SAMPLES;
        }
        self.capture_buf.drain(..offset);
        out
    }
}

impl Default for EchoCanceller {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic white noise in [-0.25, 0.25] from a linear congruential generator.
    struct Noise(u32);

    impl Noise {
        fn frame(&mut self) -> Vec<f32> {
            (0..FRAME_SAMPLES)
                .map(|_| {
                    self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((self.0 >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.5
                })
                .collect()
        }
    }

    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32
    }

    #[test]
    fn output_is_frame_aligned_and_remainder_is_kept() {
        let mut aec = EchoCanceller::new();
        assert!(aec.process_capture(&[0.0; 100]).is_empty());
        assert_eq!(aec.process_capture(&[0.0; 400]).len(), FRAME_SAMPLES);
        assert_eq!(aec.process_capture(&[0.0; 940]).len(), 2 * FRAME_SAMPLES);
    }

    #[test]
    fn capture_equal_to_render_is_attenuated() {
        // Far end and near end carry the same signal with no near-end speech,
        // so the canceller must remove most of the energy once it has adapted.
        let mut aec = EchoCanceller::new();
        let mut noise = Noise(1);
        let mut input_energy = 0.0;
        let mut output_energy = 0.0;
        for i in 0..500 {
            let frame = noise.frame();
            aec.render(&frame);
            let cleaned = aec.process_capture(&frame);
            assert_eq!(cleaned.len(), FRAME_SAMPLES);
            if i >= 400 {
                input_energy += energy(&frame);
                output_energy += energy(&cleaned);
            }
        }
        assert!(
            output_energy < input_energy * 0.25,
            "echo energy {output_energy} of input {input_energy}"
        );
    }
}
