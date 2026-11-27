//! Test fixtures: FLAC/WAV writers and engine helpers. Also used by
//! `examples/make_fixtures.rs`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use flacenc::component::BitRepr;
use flacenc::error::Verify;
use lumen_audio::{
    Backend, CaptureDevice, Engine, EngineConfig, Event, OutputConfig, SampleFormat, StreamFormat,
};

/// A fresh temporary directory for one test.
pub fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "lumen-audio-test-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Deterministic test signal: a 997 Hz sine at about -12 dBFS plus low-level
/// noise in every bit, so any bit error shows. Interleaved, right-justified ints.
pub fn signal(frames: usize, channels: usize, bits: u32, rate: u32, seed: u32) -> Vec<i32> {
    let full = f64::from(1u32 << (bits - 1));
    let mut x = seed.max(1);
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in 0..channels {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let phase =
                2.0 * std::f64::consts::PI * 997.0 * i as f64 / f64::from(rate) + ch as f64 * 0.5;
            let noise = (f64::from(x) / f64::from(u32::MAX) - 0.5) * 64.0;
            let v = (0.25 * phase.sin() * full + noise).round();
            out.push(v.clamp(-full, full - 1.0) as i32);
        }
    }
    out
}

/// Encodes `samples` (interleaved, right-justified) as FLAC.
pub fn flac_bytes(samples: &[i32], channels: usize, bits: u32, rate: u32) -> Vec<u8> {
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .expect("flac config");
    let source =
        flacenc::source::MemSource::from_samples(samples, channels, bits as usize, rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .expect("flac encode");
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).expect("flac write");
    let mut bytes = sink.as_slice().to_vec();
    // flacenc stores the short final block as the stream's minimum block size.
    // The FLAC spec excludes the last block from that field (libFLAC writes
    // min == max for fixed-size blocks), and symphonia relies on it to accept
    // fixed-blocking frame headers. Patch STREAMINFO the way libFLAC writes it.
    assert_eq!(&bytes[..4], b"fLaC");
    let (min, max) = (8..10, 10..12);
    let max_block = [bytes[max.start], bytes[max.start + 1]];
    bytes[min].copy_from_slice(&max_block);
    bytes
}

pub fn write_flac(path: &Path, samples: &[i32], channels: usize, bits: u32, rate: u32) {
    std::fs::write(path, flac_bytes(samples, channels, bits, rate)).expect("write flac");
}

/// FLAC with a Vorbis comment block (e.g. ReplayGain tags).
pub fn write_flac_with_tags(
    path: &Path,
    samples: &[i32],
    channels: usize,
    bits: u32,
    rate: u32,
    tags: &[(&str, &str)],
) {
    let bytes = flac_bytes(samples, channels, bits, rate);
    assert_eq!(&bytes[..4], b"fLaC");
    // STREAMINFO is the first block: 4-byte header + 34 bytes.
    let mut out = bytes[..4].to_vec();
    let mut streaminfo = bytes[4..4 + 4 + 34].to_vec();
    let was_last = streaminfo[0] & 0x80 != 0;
    streaminfo[0] &= 0x7F;
    out.extend_from_slice(&streaminfo);
    let mut body = Vec::new();
    let vendor = b"lumen-test";
    body.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    body.extend_from_slice(vendor);
    body.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let c = format!("{k}={v}");
        body.extend_from_slice(&(c.len() as u32).to_le_bytes());
        body.extend_from_slice(c.as_bytes());
    }
    let header = (if was_last { 0x80u8 } else { 0 }) | 4;
    out.push(header);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&body);
    out.extend_from_slice(&bytes[4 + 4 + 34..]);
    std::fs::write(path, out).expect("write flac");
}

/// Writes a PCM WAV (16/24/32-bit int).
pub fn write_wav(path: &Path, samples: &[i32], channels: u16, bits: u16, rate: u32) {
    let bytes_per = usize::from(bits / 8);
    let mut data = Vec::with_capacity(samples.len() * bytes_per);
    for s in samples {
        data.extend_from_slice(&s.to_le_bytes()[..bytes_per]);
    }
    write_wav_raw(path, &data, 1, channels, bits, rate);
}

/// Writes a 32-bit float WAV.
pub fn write_wav_f32(path: &Path, samples: &[f32], channels: u16, rate: u32) {
    let mut data = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        data.extend_from_slice(&s.to_le_bytes());
    }
    write_wav_raw(path, &data, 3, channels, 32, rate);
}

fn write_wav_raw(path: &Path, data: &[u8], tag: u16, channels: u16, bits: u16, rate: u32) {
    let block = channels * (bits / 8);
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    std::fs::write(path, out).expect("write wav");
}

/// What a device receives for a right-justified `bits` sample in an
/// integer container wide enough to hold it (left-justified i32).
pub fn device_value(sample: i32, bits: u32) -> f64 {
    f64::from(sample << (32 - bits))
}

pub const MIX_48K: StreamFormat = StreamFormat {
    rate: 48_000,
    channels: 2,
    sample: SampleFormat::Float32,
};

pub fn capture_device(
    accepts: impl Fn(&StreamFormat) -> bool + Send + Sync + 'static,
) -> Arc<CaptureDevice> {
    Arc::new(CaptureDevice::new("Test DAC", MIX_48K, accepts))
}

pub fn engine_with(device: &Arc<CaptureDevice>, exclusive: bool, volume: f32) -> Engine {
    Engine::start(EngineConfig {
        backend: Backend::Capture(device.clone()),
        output: OutputConfig {
            rate: None,
            device_id: None,
            exclusive,
        },
        volume,
        ..Default::default()
    })
    .expect("engine")
}

/// Collects events until `done` returns true (inclusive) or the timeout hits.
pub fn events_until(
    engine: &Engine,
    timeout: Duration,
    mut done: impl FnMut(&Event) -> bool,
) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match engine.events().recv_timeout(left) {
            Ok(e) => {
                let stop = done(&e);
                out.push(e);
                if stop {
                    return out;
                }
            }
            Err(_) => break,
        }
    }
    panic!("timed out waiting for event; got: {out:#?}");
}

pub fn until_end(engine: &Engine) -> Vec<Event> {
    events_until(engine, Duration::from_secs(30), |e| {
        matches!(e, Event::EndOfQueue)
    })
}
