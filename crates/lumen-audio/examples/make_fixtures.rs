//! Writes listening-test files for `lumen-play`:
//!
//! ```text
//! cargo run -p lumen-audio --example make_fixtures -- [OUT_DIR]
//! ```
//! Default `OUT_DIR` is `target/fixtures`. All tones are at -20 dBFS with
//! short fades, so they are comfortable on headphones.

#[path = "../tests/common/mod.rs"]
mod common;

use std::path::PathBuf;

/// A sine at -20 dBFS, interleaved right-justified ints. `fade` adds 20 ms
/// fades at the start/end (off for album parts, which must join seamlessly).
fn tone(
    frames: usize,
    channels: usize,
    bits: u32,
    rate: u32,
    freq: f64,
    start: usize,
    fade: bool,
) -> Vec<i32> {
    let full = f64::from(1u32 << (bits - 1));
    let fade_len = (rate as usize / 50).max(1);
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let t = (start + i) as f64 / f64::from(rate);
        let mut v = 0.1 * (2.0 * std::f64::consts::PI * freq * t).sin();
        if fade {
            let edge = i.min(frames - 1 - i);
            if edge < fade_len {
                v *= edge as f64 / fade_len as f64;
            }
        }
        let s = (v * full).round().clamp(-full, full - 1.0) as i32;
        for _ in 0..channels {
            out.push(s);
        }
    }
    out
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/fixtures"));
    let album = out.join("gapless-album");
    let rates = out.join("rate-change");
    for d in [&out, &album, &rates] {
        std::fs::create_dir_all(d).expect("create output dir");
    }

    // 24-bit / 96 kHz, 10 s, 440 Hz.
    let p = out.join("hires-24-96.flac");
    common::write_flac(
        &p,
        &tone(96_000 * 10, 2, 24, 96_000, 440.0, 0, true),
        2,
        24,
        96_000,
    );
    println!("{}", p.display());

    // One continuous 330 Hz tone cut into three tracks mid-cycle. Any gap or
    // click at the joins is clearly audible.
    let rate = 44_100;
    let lengths = [
        rate as usize * 4 + 1_234,
        rate as usize * 3 + 777,
        rate as usize * 4,
    ];
    let total: usize = lengths.iter().sum();
    let mut start = 0;
    for (n, len) in lengths.iter().enumerate() {
        let mut part = tone(*len, 2, 16, rate, 330.0, start, false);
        // Fade only the very beginning and end of the album.
        let fade = (rate as usize / 50) * 2;
        if n == 0 {
            for (i, s) in part.iter_mut().take(fade).enumerate() {
                *s = (f64::from(*s) * (i / 2) as f64 / (fade / 2) as f64) as i32;
            }
        }
        if start + len == total {
            let n_s = part.len();
            for (i, s) in part.iter_mut().skip(n_s - fade).enumerate() {
                *s = (f64::from(*s) * (1.0 - (i / 2) as f64 / (fade / 2) as f64)) as i32;
            }
        }
        let p = album.join(format!("{:02} - Part {}.flac", n + 1, n + 1));
        common::write_flac(&p, &part, 2, 16, rate);
        println!("{}", p.display());
        start += len;
    }

    // 24-bit / 384 kHz: most devices refuse this in exclusive mode, which
    // exercises the shared-mode fallback with resampling. (WAV, because the
    // test FLAC encoder stops at 96 kHz.)
    let p = out.join("ultra-24-384.wav");
    common::write_wav(
        &p,
        &tone(384_000 * 5, 2, 24, 384_000, 523.25, 0, true),
        2,
        24,
        384_000,
    );
    println!("{}", p.display());

    // Two tracks at different rates: the device reopens between them.
    for (name, r, f) in [
        ("01 - 44.1k.flac", 44_100, 392.0),
        ("02 - 48k.flac", 48_000, 440.0),
    ] {
        let p = rates.join(name);
        common::write_flac(&p, &tone(r as usize * 3, 2, 16, r, f, 0, true), 2, 16, r);
        println!("{}", p.display());
    }

    // Mono 16-bit WAV (sent as dual mono in exclusive mode).
    let p = out.join("mono-16-44.wav");
    common::write_wav(
        &p,
        &tone(44_100 * 3, 1, 16, 44_100, 600.0, 0, true),
        1,
        16,
        44_100,
    );
    println!("{}", p.display());
}
