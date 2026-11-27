//! Choosing the device format for a track.
//!
//! Exclusive mode opens the device at the file's own rate. Bit depths are
//! tried from best to worst: first every container that carries the source
//! unchanged (16-bit audio is also exact in 24- and 32-bit containers), then
//! narrower ones (the "nearest supported" fallback, which requantises with
//! dither). If the device accepts nothing at the file's rate, playback falls
//! back to shared mode and the reason is reported, so the UI can show a badge.
//! The engine never resamples in exclusive mode.

use crate::protocol::{SampleFormat, StreamFormat};

/// What the decoder delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFormat {
    pub rate: u32,
    pub channels: u16,
    /// `None` for lossy codecs.
    pub bits: Option<u32>,
}

/// One format to offer the device in exclusive mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub format: StreamFormat,
    /// True when this format carries the decoded samples unchanged.
    pub exact: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    Exclusive {
        format: StreamFormat,
        exact: bool,
    },
    Shared {
        format: StreamFormat,
        /// `(from, to)` when the source must be resampled to the mix rate.
        resample: Option<(u32, u32)>,
        /// Set when exclusive mode was requested but not possible.
        fallback: Option<String>,
    },
}

const I16: SampleFormat = SampleFormat::Int {
    container: 16,
    valid: 16,
};
const I24: SampleFormat = SampleFormat::Int {
    container: 24,
    valid: 24,
};
const I24_IN_32: SampleFormat = SampleFormat::Int {
    container: 32,
    valid: 24,
};
const I32: SampleFormat = SampleFormat::Int {
    container: 32,
    valid: 32,
};
const F32: SampleFormat = SampleFormat::Float32;

/// Device channel count in exclusive mode: mono is sent as dual mono, since
/// many drivers refuse one-channel streams.
pub fn exclusive_channels(src_channels: u16) -> u16 {
    if src_channels == 1 { 2 } else { src_channels }
}

/// True when `sample` reproduces a source of `bits` exactly.
pub fn carries_exactly(sample: SampleFormat, bits: Option<u32>) -> bool {
    match (sample, bits) {
        (SampleFormat::Int { valid, .. }, Some(b)) => u32::from(valid) >= b,
        (SampleFormat::Float32, Some(b)) => b <= 24,
        // Lossy decoders output f32.
        (SampleFormat::Float32, None) => true,
        (SampleFormat::Int { .. }, None) => false,
    }
}

/// Exclusive formats for `src`, best first.
pub fn exclusive_candidates(src: &SourceFormat) -> Vec<Candidate> {
    let order: &[SampleFormat] = match src.bits {
        Some(b) if b <= 16 => &[I16, I24_IN_32, I24, I32, F32],
        Some(b) if b <= 24 => &[I24_IN_32, I24, I32, F32, I16],
        Some(_) => &[I32, I24_IN_32, I24, F32, I16],
        None => &[F32, I24_IN_32, I24, I32, I16],
    };
    let channels = exclusive_channels(src.channels);
    order
        .iter()
        .map(|&sample| Candidate {
            format: StreamFormat {
                rate: src.rate,
                channels,
                sample,
            },
            exact: carries_exactly(sample, src.bits),
        })
        .collect()
}

/// Picks the output plan. `accepts` asks the device whether it takes a format
/// in exclusive mode; `mix` is the shared-mode mix format.
pub fn plan(
    src: &SourceFormat,
    exclusive: bool,
    mix: StreamFormat,
    mut accepts: impl FnMut(&StreamFormat) -> bool,
) -> Plan {
    if exclusive {
        for candidate in exclusive_candidates(src) {
            if accepts(&candidate.format) {
                return Plan::Exclusive {
                    format: candidate.format,
                    exact: candidate.exact,
                };
            }
        }
        let depth = src.bits.map(|b| format!("{b}-bit / ")).unwrap_or_default();
        return shared_plan(
            src,
            mix,
            Some(format!(
                "The device does not accept {depth}{} Hz in exclusive mode",
                src.rate
            )),
        );
    }
    shared_plan(src, mix, None)
}

/// Shared-mode plan: the mix format, resampling only when rates differ.
pub fn shared_plan(src: &SourceFormat, mix: StreamFormat, fallback: Option<String>) -> Plan {
    Plan::Shared {
        format: mix,
        resample: (src.rate != mix.rate).then_some((src.rate, mix.rate)),
        fallback,
    }
}

/// Whether an open exclusive stream can play `src` next without reopening
/// (and without losing bit-perfection), which keeps album playback gapless.
pub fn exclusive_stream_fits(stream: &StreamFormat, src: &SourceFormat) -> bool {
    stream.rate == src.rate
        && stream.channels == exclusive_channels(src.channels)
        && carries_exactly(stream.sample, src.bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIX: StreamFormat = StreamFormat {
        rate: 48_000,
        channels: 2,
        sample: SampleFormat::Float32,
    };

    fn src(rate: u32, bits: Option<u32>) -> SourceFormat {
        SourceFormat {
            rate,
            channels: 2,
            bits,
        }
    }

    #[test]
    fn exclusive_takes_native_format_first() {
        let p = plan(&src(96_000, Some(24)), true, MIX, |_| true);
        assert_eq!(
            p,
            Plan::Exclusive {
                format: StreamFormat {
                    rate: 96_000,
                    channels: 2,
                    sample: I24_IN_32
                },
                exact: true
            }
        );
    }

    #[test]
    fn exclusive_falls_back_to_wider_container_exactly() {
        // A device that only takes 32-bit containers still plays 16-bit exactly.
        let p = plan(&src(44_100, Some(16)), true, MIX, |f| {
            f.sample == I24_IN_32 || f.sample == I32
        });
        assert_eq!(
            p,
            Plan::Exclusive {
                format: StreamFormat {
                    rate: 44_100,
                    channels: 2,
                    sample: I24_IN_32
                },
                exact: true
            }
        );
    }

    #[test]
    fn exclusive_nearest_narrower_depth_is_marked_inexact() {
        let p = plan(&src(96_000, Some(24)), true, MIX, |f| f.sample == I16);
        assert!(matches!(p, Plan::Exclusive { exact: false, format } if format.sample == I16));
    }

    #[test]
    fn exclusive_refused_rate_falls_back_to_shared_with_reason() {
        let p = plan(&src(384_000, Some(24)), true, MIX, |f| f.rate != 384_000);
        match p {
            Plan::Shared {
                format,
                resample,
                fallback,
            } => {
                assert_eq!(format, MIX);
                assert_eq!(resample, Some((384_000, 48_000)));
                assert!(fallback.unwrap_or_default().contains("384000 Hz"));
            }
            other => panic!("expected shared, got {other:?}"),
        }
    }

    #[test]
    fn shared_mode_resamples_only_when_rates_differ() {
        assert_eq!(
            plan(&src(48_000, Some(16)), false, MIX, |_| true),
            Plan::Shared {
                format: MIX,
                resample: None,
                fallback: None
            }
        );
        assert!(matches!(
            plan(&src(44_100, Some(16)), false, MIX, |_| true),
            Plan::Shared {
                resample: Some((44_100, 48_000)),
                fallback: None,
                ..
            }
        ));
    }

    #[test]
    fn mono_is_sent_as_two_channels_in_exclusive() {
        let s = SourceFormat {
            rate: 44_100,
            channels: 1,
            bits: Some(16),
        };
        assert_eq!(exclusive_candidates(&s)[0].format.channels, 2);
    }

    #[test]
    fn lossy_prefers_float_then_24_bit() {
        let c = exclusive_candidates(&src(44_100, None));
        assert_eq!(c[0].format.sample, F32);
        assert!(c[0].exact);
        assert_eq!(c[1].format.sample, I24_IN_32);
        assert!(!c[1].exact);
    }

    #[test]
    fn stream_fit_for_gapless() {
        let s24 = StreamFormat {
            rate: 44_100,
            channels: 2,
            sample: I24_IN_32,
        };
        assert!(exclusive_stream_fits(&s24, &src(44_100, Some(16))));
        assert!(exclusive_stream_fits(&s24, &src(44_100, Some(24))));
        assert!(!exclusive_stream_fits(&s24, &src(48_000, Some(16))));
        let s16 = StreamFormat { sample: I16, ..s24 };
        assert!(!exclusive_stream_fits(&s16, &src(44_100, Some(24))));
    }
}
