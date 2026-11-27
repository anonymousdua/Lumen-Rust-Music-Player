//! Streaming sample-rate conversion with rubato, used only in shared mode
//! when the file's rate differs from the device mix rate.
//!
//! The resampler keeps running across track boundaries of the same rate, so
//! resampled albums stay gapless. Its start-up delay is trimmed from the output
//! and its tail is flushed at the end of the stream, so the output length
//! matches the input duration.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler as _};

/// Input frames per FFT chunk.
const CHUNK: usize = 1024;

pub struct Resampler {
    inner: Fft<f64>,
    channels: usize,
    from: u32,
    to: u32,
    pending: Vec<f64>,
    chunk_out: Vec<f64>,
    /// Output frames still to drop (the resampler's delay).
    drop_frames: usize,
    fed_frames: u64,
    emitted_frames: u64,
}

impl Resampler {
    pub fn new(from: u32, to: u32, channels: usize) -> Result<Self, String> {
        let inner = Fft::<f64>::new(
            from as usize,
            to as usize,
            CHUNK,
            channels,
            FixedSync::Input,
        )
        .map_err(|e| format!("cannot create resampler {from} -> {to}: {e}"))?;
        let chunk_out = vec![0.0; inner.output_frames_max() * channels];
        let drop_frames = inner.output_delay();
        Ok(Self {
            inner,
            channels,
            from,
            to,
            pending: Vec::new(),
            chunk_out,
            drop_frames,
            fed_frames: 0,
            emitted_frames: 0,
        })
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.from, self.to)
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Resamples `input` (interleaved) and appends the output to `out`.
    pub fn process(&mut self, input: &[f64], out: &mut Vec<f64>) -> Result<(), String> {
        self.fed_frames += (input.len() / self.channels) as u64;
        self.pending.extend_from_slice(input);
        while self.pending.len() / self.channels >= self.inner.input_frames_next() {
            self.run_chunk(None, out)?;
        }
        Ok(())
    }

    /// Ends the stream: processes what is left and the resampler's tail.
    pub fn flush(&mut self, out: &mut Vec<f64>) -> Result<(), String> {
        let expected =
            (self.fed_frames as f64 * f64::from(self.to) / f64::from(self.from)).round() as u64;
        let mut guard = 0;
        while self.emitted_frames < expected && guard < 64 {
            let have = self.pending.len() / self.channels;
            let partial = have.min(self.inner.input_frames_next());
            self.run_chunk(Some(partial), out)?;
            guard += 1;
        }
        if self.emitted_frames > expected {
            let extra = (self.emitted_frames - expected) as usize * self.channels;
            out.truncate(out.len().saturating_sub(extra));
            self.emitted_frames = expected;
        }
        self.reset();
        Ok(())
    }

    /// Forgets all state (after a seek).
    pub fn reset(&mut self) {
        self.inner.reset();
        self.pending.clear();
        self.drop_frames = self.inner.output_delay();
        self.fed_frames = 0;
        self.emitted_frames = 0;
    }

    fn run_chunk(&mut self, partial: Option<usize>, out: &mut Vec<f64>) -> Result<(), String> {
        let need = self.inner.input_frames_next();
        let available = self.pending.len() / self.channels;
        let take = partial.unwrap_or(need).min(available);
        // The input adapter must describe `need` frames even for a partial chunk.
        if self.pending.len() < need * self.channels {
            self.pending.resize(need * self.channels, 0.0);
        }
        let input =
            InterleavedSlice::new(&self.pending[..need * self.channels], self.channels, need)
                .map_err(|e| format!("resampler input: {e:?}"))?;
        let out_frames = self.chunk_out.len() / self.channels;
        let mut output = InterleavedSlice::new_mut(&mut self.chunk_out, self.channels, out_frames)
            .map_err(|e| format!("resampler output: {e:?}"))?;
        let indexing = partial.map(|p| Indexing::new().partial_len(p));
        let (_used, produced) = self
            .inner
            .process_into_buffer(&input, &mut output, indexing.as_ref())
            .map_err(|e| format!("resampling failed: {e}"))?;
        self.pending.drain(..take * self.channels);
        if partial.is_some() {
            // Padding added for the partial chunk is not real input.
            self.pending.clear();
        }
        let skip = self.drop_frames.min(produced);
        self.drop_frames -= skip;
        let keep = &self.chunk_out[skip * self.channels..produced * self.channels];
        out.extend_from_slice(keep);
        self.emitted_frames += (produced - skip) as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, frames: usize) -> Vec<f64> {
        (0..frames)
            .flat_map(|i| {
                let v =
                    0.5 * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn output_length_matches_duration() {
        for (from, to) in [(44_100, 48_000), (96_000, 48_000), (48_000, 192_000)] {
            let mut r = Resampler::new(from, to, 2).unwrap_or_else(|e| panic!("{e}"));
            let input = sine(from, 1000.0, from as usize); // one second
            let mut out = Vec::new();
            // Feed in odd-sized blocks like a decoder would.
            for block in input.chunks(2 * 4_321) {
                r.process(block, &mut out).unwrap_or_else(|e| panic!("{e}"));
            }
            r.flush(&mut out).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(out.len() / 2, to as usize, "{from} -> {to}");
        }
    }

    #[test]
    fn preserves_a_sine_after_delay_trim() {
        let (from, to) = (44_100u32, 48_000u32);
        let mut r = Resampler::new(from, to, 2).unwrap_or_else(|e| panic!("{e}"));
        let mut out = Vec::new();
        r.process(&sine(from, 1000.0, 44_100), &mut out)
            .unwrap_or_else(|e| panic!("{e}"));
        r.flush(&mut out).unwrap_or_else(|e| panic!("{e}"));
        let expected = sine(to, 1000.0, 48_000);
        // Compare the middle (edges have filter transients).
        let mid = 10_000 * 2..38_000 * 2;
        let err = out[mid.clone()]
            .iter()
            .zip(&expected[mid])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        assert!(err < 1e-3, "max error {err}: delay not trimmed correctly");
    }
}
