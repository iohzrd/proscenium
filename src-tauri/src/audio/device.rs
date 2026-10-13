//! Device stream format selection and sample-rate conversion shared by capture and playback.
//!
//! The codec works on 48 kHz mono f32. A device that does not offer that format is opened
//! with the closest format it supports; the stream callbacks convert sample type, channel
//! count and sample rate.

use super::codec::SAMPLE_RATE;
use cpal::{SampleFormat, StreamConfig, SupportedStreamConfig, SupportedStreamConfigRange};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Stream parameters chosen for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeviceFormat {
    pub(super) config: StreamConfig,
    pub(super) sample_format: SampleFormat,
}

impl DeviceFormat {
    /// 48 kHz mono f32: the format used when the device reports no supported format.
    pub(super) fn codec_native() -> Self {
        Self {
            config: StreamConfig {
                channels: 1,
                sample_rate: SAMPLE_RATE,
                buffer_size: cpal::BufferSize::Default,
            },
            sample_format: SampleFormat::F32,
        }
    }
}

/// Preference order of the sample formats the stream callbacks convert. Lower is preferred.
/// `None` for formats without a conversion.
fn format_rank(format: SampleFormat) -> Option<u8> {
    match format {
        SampleFormat::F32 => Some(0),
        SampleFormat::I16 => Some(1),
        SampleFormat::I32 => Some(2),
        SampleFormat::U16 => Some(3),
        SampleFormat::F64 => Some(4),
        SampleFormat::I8 => Some(5),
        SampleFormat::U8 => Some(6),
        SampleFormat::U32 => Some(7),
        _ => None,
    }
}

/// Choose a stream format from the formats a device supports.
///
/// Preference, in order: a range that contains 48 kHz, the sample format rank, the lowest
/// channel count. For a range without 48 kHz the rate is the device default rate if the
/// range contains it, otherwise the range bound nearest to 48 kHz. Falls back to `default`
/// when no range has a convertible sample format.
pub(super) fn choose_format(
    ranges: impl IntoIterator<Item = SupportedStreamConfigRange>,
    default: Option<SupportedStreamConfig>,
) -> Option<DeviceFormat> {
    let default_rate = default.as_ref().map(|d| d.sample_rate());
    let best = ranges
        .into_iter()
        .filter_map(|range| {
            let rank = format_rank(range.sample_format())?;
            if range.channels() == 0 || range.min_sample_rate() > range.max_sample_rate() {
                return None;
            }
            let native_rate = range.contains_rate(SAMPLE_RATE);
            let rate = if native_rate {
                SAMPLE_RATE
            } else {
                match default_rate {
                    Some(rate) if range.contains_rate(rate) => rate,
                    _ => SAMPLE_RATE.clamp(range.min_sample_rate(), range.max_sample_rate()),
                }
            };
            Some(((!native_rate, rank, range.channels()), range, rate))
        })
        .min_by_key(|(key, _, _)| *key);

    if let Some((_, range, rate)) = best {
        return Some(DeviceFormat {
            config: StreamConfig {
                channels: range.channels(),
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            },
            sample_format: range.sample_format(),
        });
    }

    let default = default?;
    format_rank(default.sample_format())?;
    if default.channels() == 0 || default.sample_rate() == 0 {
        return None;
    }
    Some(DeviceFormat {
        config: default.config(),
        sample_format: default.sample_format(),
    })
}

/// Streaming linear-interpolation sample-rate converter for one channel.
pub(super) struct LinearResampler {
    /// Input samples consumed per output sample.
    step: f64,
    /// Position of the next output sample between `prev` (0.0) and `cur` (1.0).
    pos: f64,
    prev: f32,
    cur: f32,
}

impl LinearResampler {
    /// Returns `None` if the rates are equal (no conversion needed) or either is zero.
    pub(super) fn new(input_rate: u32, output_rate: u32) -> Option<Self> {
        if input_rate == output_rate || input_rate == 0 || output_rate == 0 {
            return None;
        }
        Some(Self {
            step: f64::from(input_rate) / f64::from(output_rate),
            pos: 1.0,
            prev: 0.0,
            cur: 0.0,
        })
    }

    /// Push one input sample; calls `emit` for each output sample that falls before it.
    pub(super) fn push(&mut self, sample: f32, mut emit: impl FnMut(f32)) {
        self.prev = self.cur;
        self.cur = sample;
        self.pos -= 1.0;
        while self.pos < 1.0 {
            emit(self.prev + (self.cur - self.prev) * self.pos as f32);
            self.pos += self.step;
        }
    }

    /// Produce one output sample; calls `next_input` for each input sample it needs.
    pub(super) fn pull(&mut self, mut next_input: impl FnMut() -> f32) -> f32 {
        while self.pos >= 1.0 {
            self.prev = self.cur;
            self.cur = next_input();
            self.pos -= 1.0;
        }
        let out = self.prev + (self.cur - self.prev) * self.pos as f32;
        self.pos += self.step;
        out
    }
}

/// Lock a mutex, taking the guard of a poisoned mutex instead of panicking.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::SupportedBufferSize;

    fn range(
        channels: u16,
        min: u32,
        max: u32,
        format: SampleFormat,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(channels, min, max, SupportedBufferSize::Unknown, format)
    }

    #[test]
    fn prefers_48k_then_f32_then_fewest_channels() {
        let chosen = choose_format(
            [
                range(2, 44_100, 44_100, SampleFormat::F32),
                range(2, 8_000, 96_000, SampleFormat::I16),
                range(2, 48_000, 48_000, SampleFormat::F32),
                range(1, 48_000, 48_000, SampleFormat::F32),
            ],
            None,
        )
        .unwrap();
        assert_eq!(chosen, DeviceFormat::codec_native());
    }

    #[test]
    fn uses_default_rate_when_48k_is_unsupported() {
        let ranges = [range(2, 44_100, 44_100, SampleFormat::I16)];
        let default = ranges[0].with_max_sample_rate();
        let chosen = choose_format(ranges, Some(default)).unwrap();
        assert_eq!(chosen.config.sample_rate, 44_100);
        assert_eq!(chosen.config.channels, 2);
        assert_eq!(chosen.sample_format, SampleFormat::I16);

        let chosen = choose_format([range(1, 8_000, 16_000, SampleFormat::F32)], None).unwrap();
        assert_eq!(chosen.config.sample_rate, 16_000);
    }

    #[test]
    fn skips_formats_without_conversion() {
        assert!(choose_format([range(2, 48_000, 48_000, SampleFormat::I64)], None).is_none());
        let default = range(2, 48_000, 48_000, SampleFormat::I16).with_max_sample_rate();
        let chosen =
            choose_format([range(2, 48_000, 48_000, SampleFormat::I64)], Some(default)).unwrap();
        assert_eq!(chosen.sample_format, SampleFormat::I16);
    }

    #[test]
    fn resampler_output_count_matches_rate_ratio() {
        for (input_rate, output_rate) in [(44_100, 48_000), (48_000, 44_100), (16_000, 48_000)] {
            let mut resampler = LinearResampler::new(input_rate, output_rate).unwrap();
            let mut produced = 0usize;
            for _ in 0..input_rate {
                resampler.push(0.5, |_| produced += 1);
            }
            assert!(
                produced.abs_diff(output_rate as usize) <= 2,
                "{input_rate} -> {output_rate}: {produced}"
            );

            let mut resampler = LinearResampler::new(input_rate, output_rate).unwrap();
            let mut consumed = 0usize;
            for _ in 0..output_rate {
                resampler.pull(|| {
                    consumed += 1;
                    0.5
                });
            }
            assert!(
                consumed.abs_diff(input_rate as usize) <= 2,
                "{input_rate} -> {output_rate}: {consumed}"
            );
        }
        assert!(LinearResampler::new(48_000, 48_000).is_none());
    }

    #[test]
    fn resampler_interpolates_between_samples() {
        // 1:2 upsampling of a ramp yields the midpoints, delayed by one input sample.
        let mut resampler = LinearResampler::new(1, 2).unwrap();
        let mut out = Vec::new();
        for sample in [0.0, 1.0, 2.0, 3.0] {
            resampler.push(sample, |s| out.push(s));
        }
        // The first input sample is interpolated from the initial zero state.
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.5, 1.0, 1.5, 2.0, 2.5]);
    }
}
