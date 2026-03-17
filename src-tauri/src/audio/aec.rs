use aec3::voip::{VoipAec3, VoipAec3Error};

/// Number of samples per 10 ms frame at 48 kHz (mono).
const FRAME_SAMPLES: usize = 480;

/// Acoustic echo canceller wrapping `aec3::VoipAec3`.
///
/// Operates at 48 kHz mono. Buffers arbitrary-size input chunks and processes
/// them in 480-sample (10 ms) increments internally.
///
/// Usage:
/// 1. Before processing each mic chunk, call `render()` with any pending
///    playback samples to keep the far-end reference up to date.
/// 2. Call `process_capture()` with the raw mic samples; returns cleaned samples.
pub struct EchoCanceller {
    inner: VoipAec3,
    render_buf: Vec<f32>,
    capture_buf: Vec<f32>,
}

impl EchoCanceller {
    pub fn new() -> Result<Self, VoipAec3Error> {
        let inner = VoipAec3::builder(48_000, 1, 1).build()?;
        Ok(Self {
            inner,
            render_buf: Vec::new(),
            capture_buf: Vec::new(),
        })
    }

    /// Feed playback (far-end reference) samples into the AEC render path.
    /// Call this before `process_capture` for the same time slice.
    pub fn render(&mut self, samples: &[f32]) {
        self.render_buf.extend_from_slice(samples);
        while self.render_buf.len() >= FRAME_SAMPLES {
            let frame: Vec<f32> = self.render_buf.drain(..FRAME_SAMPLES).collect();
            if let Err(e) = self.inner.handle_render_frame(&frame) {
                log::warn!("[aec] render frame error: {e}");
            }
        }
    }

    /// Process mic samples through AEC. Returns echo-cancelled samples.
    /// Drain all pending render samples first by calling `render()` before this.
    pub fn process_capture(&mut self, samples: &[f32]) -> Vec<f32> {
        self.capture_buf.extend_from_slice(samples);
        let mut out = Vec::with_capacity(samples.len());
        while self.capture_buf.len() >= FRAME_SAMPLES {
            let frame: Vec<f32> = self.capture_buf.drain(..FRAME_SAMPLES).collect();
            let mut cleaned = vec![0.0f32; FRAME_SAMPLES];
            match self
                .inner
                .process_capture_frame(&frame, false, &mut cleaned)
            {
                Ok(_) => out.extend_from_slice(&cleaned),
                Err(e) => {
                    log::warn!("[aec] capture frame error: {e}");
                    out.extend_from_slice(&frame);
                }
            }
        }
        out
    }
}
