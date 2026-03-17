/// Echo cancellation stub for Phase 1 (passthrough, no-op).
/// Phase 2 will replace this with a real AEC3 implementation (sonora-aec3 or aec3-rs).
#[allow(dead_code)]
pub struct EchoCanceller;

#[allow(dead_code)]
impl EchoCanceller {
    pub fn new(_sample_rate: u32, _channels: u16) -> Self {
        Self
    }

    /// Process microphone samples against the far-end reference.
    /// Returns the input unchanged (no-op stub).
    pub fn process(&mut self, mic_samples: &[f32], _far_end: &[f32]) -> Vec<f32> {
        mic_samples.to_vec()
    }
}
