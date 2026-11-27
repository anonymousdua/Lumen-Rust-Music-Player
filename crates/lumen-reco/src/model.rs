//! What recommendations are computed from: the library snapshot, the audio
//! index and the listening signals, and the query that ranks candidates.

use std::collections::{HashMap, HashSet};

use lumen_core::TrackId;
use lumen_library::Catalog;

use crate::features::Vector;
use crate::index::AudioIndex;
use crate::score::{
    Picker, ScoreInputs, Weights, affinity, audio_similarity, log_scaled, recency_penalty, score,
    tie_key,
};
use crate::signals::Signals;

/// Seeds compared per query (more are sampled evenly).
const MAX_SEED_VECTORS: usize = 5;

/// What scoring needs to know about a track.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackMeta {
    pub id: TrackId,
    /// Every credited artist ([`split_artists`]), main artist first.
    pub artists: Vec<u32>,
    pub album: Option<u32>,
    pub play_count: u32,
    pub last_played_ms: Option<i64>,
}

/// The library as scoring sees it (rebuilt when the catalog reloads).
#[derive(Default)]
pub struct Snapshot {
    pub tracks: Vec<TrackMeta>,
    by_id: HashMap<TrackId, usize>,
    max_play_count: u32,
}

impl Snapshot {
    pub fn new(tracks: Vec<TrackMeta>) -> Self {
        let by_id = tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let max_play_count = tracks.iter().map(|t| t.play_count).max().unwrap_or(0);
        Self {
            tracks,
            by_id,
            max_play_count,
        }
    }

    pub fn from_catalog(c: &Catalog) -> Self {
        let mut names: HashMap<String, u32> = HashMap::new();
        Self::new(
            c.tracks
                .iter()
                .map(|t| TrackMeta {
                    id: t.track.id,
                    artists: split_artists(&t.track.artist)
                        .into_iter()
                        .map(|name| {
                            let next = names.len() as u32;
                            *names.entry(name).or_insert(next)
                        })
                        .collect(),
                    album: t.album.map(|a| a as u32),
                    play_count: t.track.play_count,
                    last_played_ms: t.track.last_played_ms,
                })
                .collect(),
        )
    }

    pub fn get(&self, id: TrackId) -> Option<&TrackMeta> {
        self.by_id.get(&id).map(|&i| &self.tracks[i])
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

/// A recommendation request.
#[derive(Clone, Debug, Default)]
pub struct Query<'a> {
    pub seeds: &'a [TrackId],
    pub count: usize,
    /// Never recommended (the queue, tracks already in a mix).
    pub exclude: Option<&'a HashSet<TrackId>>,
    pub now_ms: i64,
    pub skips_as_dislikes: bool,
    /// Late-night mix: per-track calmness to favour.
    pub calmness: Option<&'a HashMap<TrackId, f32>>,
}

/// One ranked candidate.
#[derive(Clone, Debug)]
pub struct Ranked {
    pub id: TrackId,
    pub score: f32,
    pub inputs: ScoreInputs,
}

pub struct Model {
    pub index: AudioIndex,
    pub signals: Signals,
    pub weights: Weights,
}

impl Model {
    /// Every candidate (not a seed, not excluded) scored, best first.
    pub fn rank(&self, snap: &Snapshot, q: &Query<'_>) -> Vec<Ranked> {
        let w = &self.weights;
        let seeds: HashSet<TrackId> = q.seeds.iter().copied().collect();
        let excluded =
            |id: &TrackId| seeds.contains(id) || q.exclude.is_some_and(|e| e.contains(id));

        // Audio: mean distance to (up to five, evenly spread) seed vectors.
        let seed_vecs: Vec<Vector> = spread(q.seeds, MAX_SEED_VECTORS)
            .into_iter()
            .filter_map(|id| self.index.get(id).copied())
            .collect();
        let distances: HashMap<TrackId, f32> = if seed_vecs.is_empty() {
            HashMap::new()
        } else {
            self.index
                .distances_to(&seed_vecs)
                .filter(|(id, _)| !excluded(id))
                .collect()
        };
        let max_distance = distances.values().copied().fold(0.0f32, f32::max);

        // Co-play with any seed.
        let mut coplay: HashMap<TrackId, u32> = HashMap::new();
        for &s in q.seeds {
            for (other, n) in self.signals.coplayed_with(s) {
                *coplay.entry(other).or_default() += n;
            }
        }
        let max_coplay = coplay.values().copied().max().unwrap_or(0) as f32;

        let seed_artists: HashSet<u32> = q
            .seeds
            .iter()
            .filter_map(|&id| snap.get(id))
            .flat_map(|t| t.artists.iter().copied())
            .collect();
        let seed_albums: HashSet<u32> = q
            .seeds
            .iter()
            .filter_map(|&id| snap.get(id).and_then(|t| t.album))
            .collect();

        let mut out: Vec<Ranked> = snap
            .tracks
            .iter()
            .filter(|t| !excluded(&t.id))
            .map(|t| {
                let stats = self.signals.stats.get(&t.id).copied().unwrap_or_default();
                let ended = (stats.last_ended_ms > 0).then_some(stats.last_ended_ms);
                let last_played = match (t.last_played_ms, ended) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                let inputs = ScoreInputs {
                    audio_similarity: distances
                        .get(&t.id)
                        .map_or(0.0, |&d| audio_similarity(d, max_distance)),
                    coplay: log_scaled(coplay.get(&t.id).copied().unwrap_or(0) as f32, max_coplay),
                    affinity: affinity(
                        w,
                        t.play_count,
                        snap.max_play_count,
                        stats.completion_rate(),
                    ),
                    same_artist_or_album: t.artists.iter().any(|a| seed_artists.contains(a))
                        || t.album.is_some_and(|a| seed_albums.contains(&a)),
                    skip_rate: stats.skip_rate(),
                    recency_penalty: recency_penalty(w, last_played, q.now_ms),
                    calmness: q.calmness.map(|c| c.get(&t.id).copied().unwrap_or(0.0)),
                };
                Ranked {
                    id: t.id,
                    score: score(w, &inputs, q.skips_as_dislikes),
                    inputs,
                }
            })
            .collect();

        let rng_seed = q.seeds.iter().fold(0x5EED_u64, |h, id| tie_key(h, id.0));
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| tie_key(rng_seed, a.id.0).cmp(&tie_key(rng_seed, b.id.0)))
        });
        out
    }

    /// The best `count` candidates that pass the diversity rules.
    pub fn recommend(&self, snap: &Snapshot, q: &Query<'_>) -> Vec<TrackId> {
        let seed_albums: Vec<u32> = q
            .seeds
            .iter()
            .filter_map(|&id| snap.get(id).and_then(|t| t.album))
            .collect();
        let mut picker = Picker::new(&self.weights, &seed_albums);
        self.pick(snap, &self.rank(snap, q), q.count, &mut picker)
    }

    /// Takes ranked candidates in order while `picker` allows them.
    pub fn pick(
        &self,
        snap: &Snapshot,
        ranked: &[Ranked],
        count: usize,
        picker: &mut Picker<'_>,
    ) -> Vec<TrackId> {
        let mut out = Vec::with_capacity(count);
        for r in ranked {
            if out.len() >= count {
                break;
            }
            let Some(t) = snap.get(r.id) else { continue };
            if picker.try_push(&t.artists, t.album) {
                out.push(r.id);
            }
        }
        out
    }
}

/// The artists credited in an artist tag, folded (case and accents) for
/// comparison: "Post Malone, Quavo", "Farak; 10A", "A feat. B", "A & B" and
/// "A / B" all name two.
pub fn split_artists(tag: &str) -> Vec<String> {
    let mut s = lumen_library::fold(tag);
    for sep in [
        " featuring ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        "(feat. ",
        "(ft. ",
    ] {
        s = s.replace(sep, ";");
    }
    let mut out: Vec<String> = Vec::new();
    for part in s.split([';', ',', '&', '/']) {
        let name = part.trim().trim_end_matches(')').trim();
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// Up to `n` items spread evenly through `items` (all of them if fewer).
fn spread(items: &[TrackId], n: usize) -> Vec<TrackId> {
    if items.len() <= n {
        return items.to_vec();
    }
    (0..n).map(|k| items[k * items.len() / n]).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::features::{DIM, Metric};
    use crate::signals::PlayRecord;

    pub fn meta(id: i64, artist: u32, album: u32) -> TrackMeta {
        TrackMeta {
            id: TrackId(id),
            artists: vec![artist],
            album: Some(album),
            play_count: 0,
            last_played_ms: None,
        }
    }

    pub fn model() -> Model {
        Model {
            index: AudioIndex::new(Metric::bliss().expect("metric")),
            signals: Signals::default(),
            weights: Weights::default(),
        }
    }

    fn v(x: f32) -> Vector {
        [x; DIM]
    }

    /// 30 tracks on a line: track k sits at k/30; 10 artists, 15 albums.
    fn line() -> (Model, Snapshot) {
        let mut m = model();
        let mut tracks = Vec::new();
        for k in 0..30 {
            m.index.insert(TrackId(k), v(k as f32 / 30.0));
            tracks.push(meta(k, (k % 10) as u32, (k / 2) as u32));
        }
        (m, Snapshot::new(tracks))
    }

    #[test]
    fn closest_sounding_first() {
        let (m, snap) = line();
        let q = Query {
            seeds: &[TrackId(10)],
            count: 4,
            now_ms: 0,
            ..Default::default()
        };
        let got = m.recommend(&snap, &q);
        // 11 is on the seed's album (allowed after the seed), then outward.
        assert_eq!(got.len(), 4);
        assert!(!got.contains(&TrackId(10)), "never the seed itself");
        let first_two: HashSet<i64> = got[..2].iter().map(|t| t.0).collect();
        assert!(first_two.contains(&11) && first_two.contains(&9), "{got:?}");
    }

    #[test]
    fn deterministic() {
        let (m, snap) = line();
        let q = Query {
            seeds: &[TrackId(3), TrackId(20)],
            count: 10,
            now_ms: 0,
            ..Default::default()
        };
        assert_eq!(m.recommend(&snap, &q), m.recommend(&snap, &q));
    }

    #[test]
    fn excludes_the_queue_and_keeps_artists_varied() {
        let (m, snap) = line();
        let queue: HashSet<TrackId> = [TrackId(11), TrackId(9)].into();
        let q = Query {
            seeds: &[TrackId(10)],
            count: 20,
            exclude: Some(&queue),
            now_ms: 0,
            ..Default::default()
        };
        let got = m.recommend(&snap, &q);
        assert!(got.iter().all(|t| !queue.contains(t)));
        for w in got.windows(10) {
            let mut per: HashMap<u32, usize> = HashMap::new();
            for t in w {
                *per.entry(snap.get(*t).map_or(99, |m| m.artists[0]))
                    .or_default() += 1;
            }
            assert!(per.values().all(|&n| n <= 2), "{got:?}");
        }
        for pair in got.windows(2) {
            let (a, b) = (
                snap.get(pair[0]).expect("track"),
                snap.get(pair[1]).expect("track"),
            );
            assert!(a.album != b.album || a.album == Some(5), "{got:?}");
        }
    }

    #[test]
    fn coplay_and_recency_move_tracks() {
        let (mut m, mut snap) = line();
        // Track 25 sounds far from the seed but was played with it often.
        for s in 0..6 {
            let base = s * 3_600_000 * 2;
            for (k, id) in [10, 25].iter().enumerate() {
                m.signals.record(
                    &PlayRecord {
                        track: TrackId(*id),
                        started_at: base + k as i64 * 200_000,
                        ended_at: base + k as i64 * 200_000 + 190_000,
                        listened_ms: 190_000,
                        duration_ms: 200_000,
                        reached_end: true,
                        interrupted: false,
                    },
                    12,
                );
            }
        }
        let now = 100 * 3_600_000;
        let q = Query {
            seeds: &[TrackId(10)],
            count: 5,
            now_ms: now,
            ..Default::default()
        };
        let position = |m: &Model| m.rank(&snap, &q).iter().position(|r| r.id == TrackId(25));
        let (fresh, _) = line();
        assert!(position(&m) < position(&fresh), "co-play lifts track 25");

        // Track 11 (the closest) was just played: it drops out.
        let i = snap
            .tracks
            .iter()
            .position(|t| t.id == TrackId(11))
            .expect("track 11");
        snap.tracks[i].last_played_ms = Some(now - 60_000);
        let snap = Snapshot::new(snap.tracks);
        assert!(!m.recommend(&snap, &q).contains(&TrackId(11)));
    }

    #[test]
    fn skips_only_count_when_enabled() {
        let (mut m, snap) = line();
        for k in 0..4 {
            m.signals.record(
                &PlayRecord {
                    track: TrackId(11),
                    started_at: k * 10_000_000,
                    ended_at: k * 10_000_000 + 5_000,
                    listened_ms: 5_000,
                    duration_ms: 200_000,
                    reached_end: false,
                    interrupted: false,
                },
                12,
            );
        }
        let now = 1_000 * 3_600_000;
        let rank_of = |skips: bool| {
            let q = Query {
                seeds: &[TrackId(10)],
                count: 30,
                now_ms: now,
                skips_as_dislikes: skips,
                ..Default::default()
            };
            m.rank(&snap, &q).iter().position(|r| r.id == TrackId(11))
        };
        assert!(rank_of(true) > rank_of(false));
    }

    #[test]
    fn artist_credits_split() {
        assert_eq!(split_artists("Post Malone"), ["post malone"]);
        assert_eq!(
            split_artists("Post Malone, Quavo"),
            ["post malone", "quavo"]
        );
        assert_eq!(
            split_artists("Farak; 10A; Saswat Balan"),
            ["farak", "10a", "saswat balan"]
        );
        assert_eq!(
            split_artists("Ed Sheeran feat. Khalid"),
            ["ed sheeran", "khalid"]
        );
        assert_eq!(
            split_artists("Timmies (feat. Shiloh Dynasty)"),
            ["timmies", "shiloh dynasty"]
        );
        assert_eq!(
            split_artists("Pritam & Arijit Singh"),
            ["pritam", "arijit singh"]
        );
        assert_eq!(split_artists("Lucía Brava"), ["lucia brava"]);
        assert!(split_artists("").is_empty());
    }

    #[test]
    fn spread_picks_evenly() {
        let ids: Vec<TrackId> = (0..10).map(TrackId).collect();
        assert_eq!(spread(&ids, 5), [0, 2, 4, 6, 8].map(TrackId));
        assert_eq!(spread(&ids[..3], 5).len(), 3);
    }
}
