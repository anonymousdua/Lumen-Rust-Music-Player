//! Sample processing: channel mapping, ReplayGain, volume and conversion to
//! the device format.
//!
//! Samples travel through the engine as interleaved `f64` in -1.0..1.0. Every
//! integer source (16/24/32-bit) and every `f32` sample is represented exactly,
//! and the conversion back multiplies by the same power of two, so a 24-bit
//! FLAC reaches a 24-bit (or wider) device unchanged when no gain is applied.

use lumen_core::ReplayGainMode;

use crate::protocol::{ReplayGainTags, SampleFormat};

/// Maps interleaved frames from `src` channels to `dst` channels.
///
/// - Same count: copied unchanged.
/// - Mono: duplicated to the first two output channels (bit-exact per channel).
/// - Fewer source channels: the extra outputs are silent.
/// - More source channels: stereo outputs get a standard downmix; otherwise
///   the first `dst` channels are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelMap {
    pub src: usize,
    pub dst: usize,
}

impl ChannelMap {
    pub fn new(src: usize, dst: usize) -> Self {
        Self {
            src: src.max(1),
            dst: dst.max(1),
        }
    }

    pub fn is_identity(&self) -> bool {
        self.src == self.dst
    }

    /// Appends the mapped frames of `input` to `out`.
    pub fn apply(&self, input: &[f64], out: &mut Vec<f64>) {
        let frames = input.len() / self.src;
        if self.is_identity() {
            out.extend_from_slice(&input[..frames * self.src]);
            return;
        }
        out.reserve(frames * self.dst);
        for frame in input.chunks_exact(self.src) {
            match (self.src, self.dst) {
                (1, _) => {
                    for ch in 0..self.dst {
                        out.push(if ch < 2 { frame[0] } else { 0.0 });
                    }
                }
                (s, d) if s < d => {
                    out.extend_from_slice(frame);
                    out.extend(std::iter::repeat_n(0.0, d - s));
                }
                (s, 2) if s > 2 => {
                    // ITU-style downmix of the common 5.1 layout (L R C LFE Ls Rs);
                    // other layouts fold extra channels in equally.
                    let (mut l, mut r) = (frame[0], frame[1]);
                    const K: f64 = std::f64::consts::FRAC_1_SQRT_2;
                    if s >= 6 {
                        l += K * frame[2] + K * frame[4];
                        r += K * frame[2] + K * frame[5];
                    } else {
                        for (i, &v) in frame.iter().enumerate().skip(2) {
                            if i % 2 == 0 {
                                l += K * v;
                            } else {
                                r += K * v;
                            }
                        }
                    }
                    let norm = 1.0 / (1.0 + 2.0 * K);
                    out.push(l * norm);
                    out.push(r * norm);
                }
                (_, d) => out.extend_from_slice(&frame[..d]),
            }
        }
    }
}

/// Parses a ReplayGain gain tag such as `"-6.48 dB"` or `"+1.2"`.
pub fn parse_gain_db(s: &str) -> Option<f32> {
    let t = s.trim();
    let t = t
        .strip_suffix("dB")
        .or_else(|| t.strip_suffix("db"))
        .or_else(|| t.strip_suffix("DB"))
        .unwrap_or(t)
        .trim();
    let t = t.replace('\u{2212}', "-"); // Unicode minus
    let v: f32 = t.trim_start_matches('+').parse().ok()?;
    v.is_finite().then_some(v)
}

/// Parses a ReplayGain peak tag (linear, 1.0 = full scale).
pub fn parse_peak(s: &str) -> Option<f32> {
    let v: f32 = s.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then_some(v)
}

/// Linear factor and dB to apply for `mode`, limited so the tagged peak does
/// not clip. `None` means "leave samples untouched".
pub fn replaygain_factor(tags: &ReplayGainTags, mode: ReplayGainMode) -> Option<(f64, f32)> {
    let (gain, peak) = match mode {
        ReplayGainMode::Off => return None,
        ReplayGainMode::Track => (tags.track_gain_db, tags.track_peak),
        ReplayGainMode::Album => (
            tags.album_gain_db.or(tags.track_gain_db),
            tags.album_peak.or(tags.track_peak),
        ),
    };
    let mut db = gain?;
    if let Some(peak) = peak {
        // Highest gain that keeps peak * gain <= 1.0.
        let max_db = -20.0 * peak.log10();
        if db > max_db {
            db = max_db;
        }
    }
    if db == 0.0 {
        return None;
    }
    Some((10f64.powf(f64::from(db) / 20.0), db))
}

/// Volume with a short linear ramp so changes don't click.
#[derive(Clone, Debug)]
pub struct GainRamp {
    current: f64,
    target: f64,
    step: f64,
}

impl GainRamp {
    pub fn new(gain: f64) -> Self {
        Self {
            current: gain,
            target: gain,
            step: 0.0,
        }
    }

    /// Ramp to `target` over `frames` frames.
    pub fn set_target(&mut self, target: f64, frames: usize) {
        if target == self.target {
            return;
        }
        self.target = target;
        self.step = (target - self.current) / frames.max(1) as f64;
    }

    /// Jump straight to `target` (used while silent).
    pub fn snap(&mut self, target: f64) {
        self.current = target;
        self.target = target;
        self.step = 0.0;
    }

    pub fn is_unity(&self) -> bool {
        self.current == 1.0 && self.target == 1.0
    }

    #[inline]
    fn next_frame(&mut self) -> f64 {
        if self.current != self.target {
            let next = self.current + self.step;
            let done = (self.step > 0.0 && next >= self.target)
                || (self.step < 0.0 && next <= self.target)
                || self.step == 0.0;
            self.current = if done { self.target } else { next };
        }
        self.current
    }
}

/// Writes `f64` samples into a device buffer in a given [`SampleFormat`].
#[derive(Clone, Debug)]
pub struct Converter {
    format: SampleFormat,
    channels: usize,
    rng: u32,
}

impl Converter {
    pub fn new(format: SampleFormat, channels: usize) -> Self {
        Self {
            format,
            channels: channels.max(1),
            rng: 0x1234_5678,
        }
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.format.container_bytes() * self.channels
    }

    /// Converts the interleaved `samples` (whole frames) into `out`, which must
    /// hold `samples.len() * container_bytes` bytes. Applies `gain` per frame.
    /// Never allocates (safe on the real-time thread).
    pub fn write(&mut self, samples: &[f64], gain: &mut GainRamp, out: &mut [u8]) {
        let unity = gain.is_unity();
        match self.format {
            SampleFormat::Float32 => {
                for (frame_in, frame_out) in samples
                    .chunks_exact(self.channels)
                    .zip(out.chunks_exact_mut(4 * self.channels))
                {
                    let g = if unity { 1.0 } else { gain.next_frame() };
                    for (s, o) in frame_in.iter().zip(frame_out.as_chunks_mut::<4>().0) {
                        *o = ((s * g) as f32).to_le_bytes();
                    }
                }
            }
            SampleFormat::Int { container, valid } => {
                let bytes = usize::from(container / 8);
                let scale = f64::from(1u32 << (valid.clamp(8, 32) - 1));
                let max = scale - 1.0;
                let min = -scale;
                let shift = u32::from(container - valid);
                // Quantising to 16 bits or less after a gain change gets TPDF dither.
                let dither = !unity && valid <= 16;
                for (frame_in, frame_out) in samples
                    .chunks_exact(self.channels)
                    .zip(out.chunks_exact_mut(bytes * self.channels))
                {
                    let g = if unity { 1.0 } else { gain.next_frame() };
                    for (s, o) in frame_in.iter().zip(frame_out.chunks_exact_mut(bytes)) {
                        let mut v = s * g * scale;
                        if dither {
                            v += self.tpdf();
                        }
                        let q = v.round().clamp(min, max) as i64;
                        let word = (q << shift) as i32;
                        o.copy_from_slice(&word.to_le_bytes()[..bytes]);
                    }
                }
            }
        }
    }

    /// Triangular dither in LSB units (-1..1).
    #[inline]
    fn tpdf(&mut self) -> f64 {
        let a = self.next_rand();
        let b = self.next_rand();
        a - b
    }

    #[inline]
    fn next_rand(&mut self) -> f64 {
        // xorshift32
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        f64::from(x) / f64::from(u32::MAX)
    }
}

/// Reads one device sample back to a left-justified `i32` (tests and capture).
pub fn read_int_sample(bytes: &[u8]) -> i32 {
    let mut word = [0u8; 4];
    let n = bytes.len().min(4);
    word[4 - n..].copy_from_slice(&bytes[..n]);
    i32::from_le_bytes(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(samples: &[f64], format: SampleFormat, channels: usize, gain: f64) -> Vec<u8> {
        let mut c = Converter::new(format, channels);
        let mut out = vec![0u8; samples.len() * format.container_bytes()];
        c.write(samples, &mut GainRamp::new(gain), &mut out);
        out
    }

    #[test]
    fn int24_round_trips_bit_exactly_through_f64() {
        // Every 24-bit value pattern of interest, left-justified like symphonia's FLAC output.
        let mut values: Vec<i32> = vec![-(1 << 23), (1 << 23) - 1, 0, 1, -1, 4_660_947, -7_000_001];
        let mut x: u32 = 99;
        for _ in 0..10_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            values.push(((x as i32) >> 8).clamp(-(1 << 23), (1 << 23) - 1));
        }
        let decoded: Vec<f64> = values
            .iter()
            .map(|&v| f64::from(v << 8) / 2_147_483_648.0)
            .collect();
        for format in [
            SampleFormat::Int {
                container: 32,
                valid: 24,
            },
            SampleFormat::Int {
                container: 24,
                valid: 24,
            },
            SampleFormat::Int {
                container: 32,
                valid: 32,
            },
        ] {
            let bytes = convert(&decoded, format, 1, 1.0);
            let n = format.container_bytes();
            for (v, chunk) in values.iter().zip(bytes.chunks_exact(n)) {
                assert_eq!(read_int_sample(chunk), v << 8, "{format}");
            }
        }
    }

    #[test]
    fn int16_source_is_exact_in_16_and_wider_containers() {
        let values: Vec<i16> = vec![i16::MIN, -1, 0, 1, 12_345, i16::MAX];
        let decoded: Vec<f64> = values.iter().map(|&v| f64::from(v) / 32_768.0).collect();
        let bytes = convert(
            &decoded,
            SampleFormat::Int {
                container: 16,
                valid: 16,
            },
            1,
            1.0,
        );
        for (v, c) in values.iter().zip(bytes.as_chunks::<2>().0) {
            assert_eq!(i16::from_le_bytes(*c), *v);
        }
        let bytes = convert(
            &decoded,
            SampleFormat::Int {
                container: 32,
                valid: 24,
            },
            1,
            1.0,
        );
        for (v, c) in values.iter().zip(bytes.as_chunks::<4>().0) {
            assert_eq!(read_int_sample(c), i32::from(*v) << 16);
        }
        let bytes = convert(&decoded, SampleFormat::Float32, 1, 1.0);
        for (v, c) in values.iter().zip(bytes.as_chunks::<4>().0) {
            let f = f32::from_le_bytes(*c);
            assert_eq!(f64::from(f), f64::from(*v) / 32_768.0);
        }
    }

    #[test]
    fn full_scale_positive_clamps() {
        let bytes = convert(
            &[1.0, -1.0],
            SampleFormat::Int {
                container: 16,
                valid: 16,
            },
            1,
            1.0,
        );
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), i16::MIN);
    }

    #[test]
    fn gain_scales_samples() {
        let bytes = convert(
            &[0.5],
            SampleFormat::Int {
                container: 32,
                valid: 32,
            },
            1,
            0.5,
        );
        assert_eq!(read_int_sample(&bytes), 1 << 29);
    }

    #[test]
    fn gain_ramp_reaches_target() {
        let mut g = GainRamp::new(1.0);
        g.set_target(0.0, 4);
        let v: Vec<f64> = (0..6).map(|_| g.next_frame()).collect();
        assert_eq!(v, vec![0.75, 0.5, 0.25, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn channel_maps() {
        let mut out = Vec::new();
        ChannelMap::new(1, 2).apply(&[0.1, 0.2], &mut out);
        assert_eq!(out, vec![0.1, 0.1, 0.2, 0.2]);
        out.clear();
        ChannelMap::new(2, 4).apply(&[0.1, 0.2], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.0, 0.0]);
        out.clear();
        ChannelMap::new(2, 2).apply(&[0.1, 0.2, 0.3, 0.4], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
        out.clear();
        ChannelMap::new(6, 2).apply(&[0.0, 0.0, 0.5, 0.9, 0.0, 0.0], &mut out);
        assert!(
            (out[0] - out[1]).abs() < 1e-12 && out[0] > 0.0,
            "centre goes to both sides"
        );
    }

    #[test]
    fn replaygain_parsing() {
        assert_eq!(parse_gain_db("-6.48 dB"), Some(-6.48));
        assert_eq!(parse_gain_db("+1.50 dB"), Some(1.5));
        assert_eq!(parse_gain_db(" 2.0"), Some(2.0));
        assert_eq!(parse_gain_db("\u{2212}3.0 dB"), Some(-3.0));
        assert_eq!(parse_gain_db("loud"), None);
        assert_eq!(parse_peak("0.988"), Some(0.988));
        assert_eq!(parse_peak("0"), None);
    }

    #[test]
    fn replaygain_respects_mode_and_peak() {
        let tags = ReplayGainTags {
            track_gain_db: Some(-6.0),
            track_peak: Some(0.9),
            album_gain_db: Some(4.0),
            album_peak: Some(0.95),
        };
        assert_eq!(replaygain_factor(&tags, ReplayGainMode::Off), None);
        let (f, db) = replaygain_factor(&tags, ReplayGainMode::Track).unwrap_or((0.0, 0.0));
        assert!((db + 6.0).abs() < 1e-6 && (f - 0.501_187).abs() < 1e-5);
        // +4 dB would clip a 0.95 peak; limited to about +0.45 dB.
        let (f, db) = replaygain_factor(&tags, ReplayGainMode::Album).unwrap_or((0.0, 0.0));
        assert!(db < 0.5 && db > 0.4, "{db}");
        assert!(f * 0.95 <= 1.000_001);
        // Album mode falls back to track gain.
        let only_track = ReplayGainTags {
            track_gain_db: Some(-3.0),
            ..Default::default()
        };
        assert!(replaygain_factor(&only_track, ReplayGainMode::Album).is_some());
        assert_eq!(
            replaygain_factor(&ReplayGainTags::default(), ReplayGainMode::Track),
            None
        );
    }
}
