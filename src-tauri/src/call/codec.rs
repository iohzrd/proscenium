use opus::{Channels, Decoder, Encoder};

/// Opus sample rate (48 kHz, mandatory for Opus).
pub const SAMPLE_RATE: u32 = 48_000;
/// Frame duration in milliseconds. 20 ms is the sweet spot for latency vs overhead.
pub const FRAME_DURATION_MS: u32 = 20;
/// Samples per channel per frame: 48000 * 20 / 1000 = 960.
pub const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE * FRAME_DURATION_MS / 1000) as usize;
/// We operate in mono for voice calls.
pub const CHANNELS: Channels = Channels::Mono;
/// Total samples per frame (mono = same as SAMPLES_PER_FRAME).
pub const FRAME_SIZE: usize = SAMPLES_PER_FRAME;
/// Maximum Opus packet size in bytes.
const MAX_PACKET_SIZE: usize = 4000;

pub struct OpusEncoder {
    encoder: Encoder,
    /// Accumulation buffer for incoming samples (may arrive in non-frame-aligned chunks).
    buffer: Vec<f32>,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, opus::Error> {
        let encoder = Encoder::new(SAMPLE_RATE, CHANNELS, opus::Application::Voip)?;
        Ok(Self {
            encoder,
            buffer: Vec::with_capacity(FRAME_SIZE * 2),
        })
    }

    /// Push samples into the encoder buffer. Returns encoded Opus packets
    /// for each complete frame accumulated.
    pub fn push_samples(&mut self, samples: &[f32]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(samples);
        let mut packets = Vec::new();
        while self.buffer.len() >= FRAME_SIZE {
            let frame: Vec<f32> = self.buffer.drain(..FRAME_SIZE).collect();
            let mut output = vec![0u8; MAX_PACKET_SIZE];
            match self.encoder.encode_float(&frame, &mut output) {
                Ok(len) => {
                    output.truncate(len);
                    packets.push(output);
                }
                Err(e) => log::error!("[opus-enc] encode error: {e}"),
            }
        }
        packets
    }
}

pub struct OpusDecoder {
    decoder: Decoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, opus::Error> {
        let decoder = Decoder::new(SAMPLE_RATE, CHANNELS)?;
        Ok(Self { decoder })
    }

    /// Decode an Opus packet into f32 samples. Returns the decoded samples.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, opus::Error> {
        let mut output = vec![0.0f32; FRAME_SIZE];
        let decoded = self.decoder.decode_float(packet, &mut output, false)?;
        output.truncate(decoded);
        Ok(output)
    }

    /// Generate comfort noise / silence for a lost packet (PLC).
    #[allow(dead_code)] // available for future jitter buffer use
    pub fn decode_loss(&mut self) -> Result<Vec<f32>, opus::Error> {
        let mut output = vec![0.0f32; FRAME_SIZE];
        let decoded = self.decoder.decode_float(&[], &mut output, false)?;
        output.truncate(decoded);
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let mut enc = OpusEncoder::new().unwrap();
        let mut dec = OpusDecoder::new().unwrap();

        // Generate a 960-sample sine wave (one frame)
        let samples: Vec<f32> = (0..FRAME_SIZE)
            .map(|i| (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / SAMPLE_RATE as f32).sin())
            .collect();

        let packets = enc.push_samples(&samples);
        assert_eq!(packets.len(), 1);

        let decoded = dec.decode(&packets[0]).unwrap();
        assert_eq!(decoded.len(), FRAME_SIZE);
        // Lossy codec, so just check non-silence
        let energy: f32 = decoded.iter().map(|s| s * s).sum();
        assert!(energy > 0.01);
    }

    #[test]
    fn encode_accumulates_partial_frames() {
        let mut enc = OpusEncoder::new().unwrap();

        // Push half a frame
        let half: Vec<f32> = vec![0.0; FRAME_SIZE / 2];
        let packets = enc.push_samples(&half);
        assert!(packets.is_empty());

        // Push the other half
        let packets = enc.push_samples(&half);
        assert_eq!(packets.len(), 1);
    }

    #[test]
    fn decode_loss_produces_samples() {
        let mut dec = OpusDecoder::new().unwrap();
        let samples = dec.decode_loss().unwrap();
        assert_eq!(samples.len(), FRAME_SIZE);
    }

    #[test]
    fn encode_multiple_frames() {
        let mut enc = OpusEncoder::new().unwrap();
        let samples: Vec<f32> = vec![0.1; FRAME_SIZE * 3];
        let packets = enc.push_samples(&samples);
        assert_eq!(packets.len(), 3);
    }
}
