//! Recommendation query time with 10,000 synthetic tracks.
//!
//! `cargo run --release -p lumen-reco --example bench_query`

use std::collections::HashSet;
use std::time::{Duration, Instant};

use lumen_core::TrackId;
use lumen_reco::features::{DIM, Metric, Vector};
use lumen_reco::index::AudioIndex;
use lumen_reco::signals::{PlayRecord, Signals};
use lumen_reco::{MixKind, Model, Query, Snapshot, TrackMeta, Weights};

const N: i64 = 10_000;
const RUNS: usize = 200;

fn main() {
    let mut rng: u64 = 0x1234_5678;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut index = AudioIndex::new(Metric::bliss().expect("metric"));
    let mut tracks = Vec::new();
    for id in 0..N {
        let mut v: Vector = [0.0; DIM];
        for x in &mut v {
            *x = (next() % 2000) as f32 / 1000.0 - 1.0;
        }
        index.insert(TrackId(id), v);
        tracks.push(TrackMeta {
            id: TrackId(id),
            artists: vec![(id % 1500) as u32],
            album: Some((id / 10) as u32),
            play_count: (next() % 40) as u32,
            last_played_ms: Some((next() % 1_000_000_000) as i64),
        });
    }
    // A year of listening: 20,000 plays in sessions of about 15.
    let mut signals = Signals::default();
    let mut t = 0i64;
    for k in 0..20_000 {
        if k % 15 == 0 {
            t += 3 * 3_600_000;
        }
        let listened = 20 + next() % 200;
        signals.record(
            &PlayRecord {
                track: TrackId((next() % N as u64) as i64),
                started_at: t,
                ended_at: t + listened as i64 * 1000,
                listened_ms: listened * 1000,
                duration_ms: 200_000,
                reached_end: listened > 190,
                interrupted: false,
            },
            (next() % 24) as u8,
        );
        t += 210_000;
    }
    let snap = Snapshot::new(tracks);
    let model = Model {
        index,
        signals,
        weights: Weights::default(),
    };
    let queue: HashSet<TrackId> = (0..50).map(|k| TrackId(k * 7)).collect();
    let now = t + 3_600_000;

    let time = |label: &str, f: &dyn Fn() -> usize| {
        let mut times: Vec<Duration> = (0..RUNS)
            .map(|_| {
                let s = Instant::now();
                std::hint::black_box(f());
                s.elapsed()
            })
            .collect();
        times.sort();
        println!(
            "{label:<38} median {:>7.3} ms   p95 {:>7.3} ms",
            times[RUNS / 2].as_secs_f64() * 1000.0,
            times[RUNS * 95 / 100].as_secs_f64() * 1000.0
        );
    };

    println!("{N} tracks, {} co-play links\n", 20_000);
    let one = [TrackId(4242)];
    time("Play similar (1 seed, 25 tracks)", &|| {
        model
            .recommend(
                &snap,
                &Query {
                    seeds: &one,
                    count: 24,
                    exclude: Some(&queue),
                    now_ms: now,
                    skips_as_dislikes: true,
                    calmness: None,
                },
            )
            .len()
    });
    let three = [TrackId(1), TrackId(500), TrackId(9000)];
    time("Radio (3 seeds, 10 tracks)", &|| {
        model
            .recommend(
                &snap,
                &Query {
                    seeds: &three,
                    count: 10,
                    exclude: Some(&queue),
                    now_ms: now,
                    skips_as_dislikes: true,
                    calmness: None,
                },
            )
            .len()
    });
    let album: Vec<TrackId> = (4240..4250).map(TrackId).collect();
    time("Play similar on an album (10 seeds)", &|| {
        model
            .recommend(
                &snap,
                &Query {
                    seeds: &album,
                    count: 24,
                    exclude: Some(&queue),
                    now_ms: now,
                    skips_as_dislikes: true,
                    calmness: None,
                },
            )
            .len()
    });
    time("Nearest neighbours only (k = 5)", &|| {
        model.index.nearest(TrackId(4242), 5).len()
    });
    time("Late-night mix (24)", &|| {
        MixKind::LateNight.build(&model, &snap, now, true).len()
    });
    time("Rediscover (30)", &|| {
        MixKind::Rediscover.build(&model, &snap, now, true).len()
    });
}
