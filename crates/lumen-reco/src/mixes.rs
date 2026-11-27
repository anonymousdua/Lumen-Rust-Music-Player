//! "Made for you" mixes: Late-night mix and Rediscover.

use std::collections::HashSet;

use lumen_core::TrackId;

use crate::model::{Model, Query, Snapshot};
use crate::score::{Picker, affinity};

const DAY_MS: i64 = 24 * 3_600_000;
/// Late-night mix: seeds taken from the most-played late-night tracks.
const LATE_NIGHT_SEEDS: usize = 5;
/// Rediscover: favourites not played for this long.
pub const REDISCOVER_AFTER_MS: i64 = 60 * DAY_MS;
/// Rediscover: a favourite needs at least this many plays.
const REDISCOVER_MIN_PLAYS: u32 = 2;
/// Rediscover: nearest neighbours after each favourite.
const REDISCOVER_NEIGHBOURS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MixKind {
    LateNight,
    Rediscover,
}

impl MixKind {
    pub const ALL: [MixKind; 2] = [MixKind::LateNight, MixKind::Rediscover];

    pub fn name(self) -> &'static str {
        match self {
            MixKind::LateNight => "Late-night mix",
            MixKind::Rediscover => "Rediscover",
        }
    }

    /// Key in the `mixes` table.
    pub fn key(self) -> &'static str {
        match self {
            MixKind::LateNight => "late_night",
            MixKind::Rediscover => "rediscover",
        }
    }

    pub fn size(self) -> usize {
        match self {
            MixKind::LateNight => 24,
            MixKind::Rediscover => 30,
        }
    }

    /// Shown when the mix has nothing in it yet.
    pub fn empty_hint(self) -> &'static str {
        match self {
            MixKind::LateNight => {
                "Play some music and Lumen will build this mix from what you play after 9 pm."
            }
            MixKind::Rediscover => {
                "Songs you played a lot but haven't heard in 60 days will show up here."
            }
        }
    }

    pub fn build(self, model: &Model, snap: &Snapshot, now_ms: i64, skips: bool) -> Vec<TrackId> {
        match self {
            MixKind::LateNight => late_night(model, snap, now_ms, skips),
            MixKind::Rediscover => rediscover(model, snap, now_ms, skips),
        }
    }
}

/// Tracks you play after 9 pm, most often first. Without late-night history
/// yet: the calmest tracks you have played.
pub fn late_night_seeds(model: &Model, snap: &Snapshot) -> Vec<TrackId> {
    let mut late: Vec<(TrackId, u32)> = model
        .signals
        .stats
        .iter()
        .filter(|(id, s)| s.late_night > 0 && snap.get(**id).is_some())
        .map(|(id, s)| (*id, s.late_night))
        .collect();
    late.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if !late.is_empty() {
        return late
            .into_iter()
            .take(LATE_NIGHT_SEEDS)
            .map(|l| l.0)
            .collect();
    }
    let calm = model.index.calmness();
    let mut played: Vec<(TrackId, f32)> = snap
        .tracks
        .iter()
        .filter(|t| t.play_count > 0)
        .filter_map(|t| calm.get(&t.id).map(|&c| (t.id, c)))
        .collect();
    played.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    played
        .into_iter()
        .take(LATE_NIGHT_SEEDS)
        .map(|p| p.0)
        .collect()
}

/// 24 tracks like your late-night ones, leaning slower and quieter.
pub fn late_night(model: &Model, snap: &Snapshot, now_ms: i64, skips: bool) -> Vec<TrackId> {
    let seeds = late_night_seeds(model, snap);
    if seeds.is_empty() {
        return Vec::new();
    }
    let calm = model.index.calmness();
    model.recommend(
        snap,
        &Query {
            seeds: &seeds,
            count: MixKind::LateNight.size(),
            exclude: None,
            now_ms,
            skips_as_dislikes: skips,
            calmness: Some(&calm),
        },
    )
}

/// Favourites (2+ plays) not played in 60 days, highest affinity first.
pub fn rediscover_anchors(model: &Model, snap: &Snapshot, now_ms: i64) -> Vec<TrackId> {
    let max_plays = snap.tracks.iter().map(|t| t.play_count).max().unwrap_or(0);
    let mut anchors: Vec<(TrackId, f32)> = snap
        .tracks
        .iter()
        .filter(|t| t.play_count >= REDISCOVER_MIN_PLAYS)
        .filter_map(|t| {
            let stats = model.signals.stats.get(&t.id).copied().unwrap_or_default();
            let ended = (stats.last_ended_ms > 0).then_some(stats.last_ended_ms);
            let last = match (t.last_played_ms, ended) {
                (Some(a), Some(b)) => a.max(b),
                (a, b) => a.or(b)?,
            };
            (now_ms - last >= REDISCOVER_AFTER_MS).then(|| {
                let a = affinity(
                    &model.weights,
                    t.play_count,
                    max_plays,
                    stats.completion_rate(),
                );
                (t.id, a)
            })
        })
        .collect();
    anchors.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    anchors.into_iter().map(|a| a.0).collect()
}

/// Each forgotten favourite followed by its two nearest neighbours.
pub fn rediscover(model: &Model, snap: &Snapshot, now_ms: i64, skips: bool) -> Vec<TrackId> {
    let size = MixKind::Rediscover.size();
    let anchors = rediscover_anchors(model, snap, now_ms);
    let anchor_albums: Vec<u32> = anchors
        .iter()
        .filter_map(|&id| snap.get(id).and_then(|t| t.album))
        .collect();
    let mut picker = Picker::new(&model.weights, &anchor_albums);
    let mut used: HashSet<TrackId> = anchors.iter().copied().collect();
    let mut out = Vec::with_capacity(size);
    for &anchor in &anchors {
        if out.len() >= size {
            break;
        }
        let Some(t) = snap.get(anchor) else { continue };
        if !picker.try_push(&t.artists, t.album) {
            continue;
        }
        out.push(anchor);
        let q = Query {
            seeds: std::slice::from_ref(&anchor),
            count: REDISCOVER_NEIGHBOURS,
            exclude: Some(&used),
            now_ms,
            skips_as_dislikes: skips,
            calmness: None,
        };
        let ranked = model.rank(snap, &q);
        let room = REDISCOVER_NEIGHBOURS.min(size - out.len());
        let next = model.pick(snap, &ranked, room, &mut picker);
        used.extend(next.iter().copied());
        out.extend(next);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{DIM, LOUDNESS, TEMPO, Vector};
    use crate::model::TrackMeta;
    use crate::model::tests::{meta, model};
    use crate::signals::PlayRecord;

    fn v(x: f32) -> Vector {
        [x; DIM]
    }

    fn play(m: &mut Model, id: i64, at: i64, hour: u8) {
        m.signals.record(
            &PlayRecord {
                track: TrackId(id),
                started_at: at,
                ended_at: at + 190_000,
                listened_ms: 190_000,
                duration_ms: 200_000,
                reached_end: true,
                interrupted: false,
            },
            hour,
        );
    }

    #[test]
    fn late_night_seeds_come_from_plays_after_nine() {
        let mut m = model();
        let mut tracks = Vec::new();
        for k in 0..20 {
            m.index.insert(TrackId(k), v(k as f32 / 20.0));
            tracks.push(meta(k, k as u32, k as u32));
        }
        let snap = Snapshot::new(tracks);
        play(&mut m, 3, 0, 14);
        play(&mut m, 7, DAY_MS, 22);
        play(&mut m, 7, 2 * DAY_MS, 23);
        play(&mut m, 12, 3 * DAY_MS, 1);
        assert_eq!(late_night_seeds(&m, &snap), [TrackId(7), TrackId(12)]);
        let mix = late_night(&m, &snap, 10 * DAY_MS, true);
        assert_eq!(mix.len(), 18, "every other track: 20 minus the 2 seeds");
        assert!(!mix.contains(&TrackId(7)) && !mix.contains(&TrackId(12)));
    }

    #[test]
    fn late_night_leans_calm() {
        let mut m = model();
        // Two candidates equally far from the seed: one calm, one loud.
        let seed = v(0.0);
        let mut calm = v(0.0);
        calm[TEMPO] = -0.5;
        calm[LOUDNESS] = -0.5;
        let mut loud = v(0.0);
        loud[TEMPO] = 0.5;
        loud[LOUDNESS] = 0.5;
        m.index.insert(TrackId(1), seed);
        m.index.insert(TrackId(2), loud);
        m.index.insert(TrackId(3), calm);
        let snap = Snapshot::new(vec![meta(1, 1, 1), meta(2, 2, 2), meta(3, 3, 3)]);
        play(&mut m, 1, 0, 23);
        assert_eq!(late_night(&m, &snap, DAY_MS, true)[0], TrackId(3));
    }

    #[test]
    fn late_night_falls_back_to_calm_played_tracks() {
        let mut m = model();
        let mut a = v(0.0);
        a[TEMPO] = 0.9;
        let mut b = v(0.0);
        b[TEMPO] = -0.9;
        m.index.insert(TrackId(1), a);
        m.index.insert(TrackId(2), b);
        m.index.insert(TrackId(3), v(0.0));
        let mut t1 = meta(1, 1, 1);
        t1.play_count = 3;
        let mut t2 = meta(2, 2, 2);
        t2.play_count = 1;
        let snap = Snapshot::new(vec![t1, t2, meta(3, 3, 3)]);
        assert_eq!(late_night_seeds(&m, &snap), [TrackId(2), TrackId(1)]);
        let unplayed = Snapshot::new(vec![meta(1, 1, 1), meta(2, 2, 2)]);
        assert!(late_night(&m, &unplayed, 0, true).is_empty());
    }

    fn favourite(id: i64, plays: u32, last: Option<i64>) -> TrackMeta {
        TrackMeta {
            play_count: plays,
            last_played_ms: last,
            ..meta(id, id as u32, id as u32)
        }
    }

    #[test]
    fn rediscover_anchors_are_old_favourites() {
        let m = model();
        let now = 400 * DAY_MS;
        let snap = Snapshot::new(vec![
            favourite(1, 9, Some(now - 90 * DAY_MS)),
            favourite(2, 9, Some(now - 10 * DAY_MS)), // too recent
            favourite(3, 1, Some(now - 90 * DAY_MS)), // only one play
            favourite(4, 4, Some(now - 61 * DAY_MS)),
            favourite(5, 4, None), // never played
            favourite(6, 20, Some(now - 200 * DAY_MS)),
        ]);
        assert_eq!(
            rediscover_anchors(&m, &snap, now),
            [TrackId(6), TrackId(1), TrackId(4)]
        );
    }

    #[test]
    fn rediscover_follows_each_favourite_with_neighbours() {
        let mut m = model();
        let now = 400 * DAY_MS;
        let mut tracks = Vec::new();
        // Favourites at 0 and 100; each has two close neighbours.
        for (id, x) in [
            (0, 0.0),
            (1, 0.01),
            (2, -0.01),
            (100, 0.9),
            (101, 0.91),
            (102, 0.89),
        ] {
            m.index.insert(TrackId(id), v(x));
            tracks.push(meta(id, id as u32, id as u32));
        }
        tracks[0] = favourite(0, 10, Some(now - 70 * DAY_MS));
        tracks[3] = favourite(100, 5, Some(now - 70 * DAY_MS));
        let snap = Snapshot::new(tracks);
        let mix = rediscover(&m, &snap, now, true);
        assert_eq!(mix.len(), 6);
        assert_eq!(mix[0], TrackId(0));
        let after_first: HashSet<i64> = mix[1..3].iter().map(|t| t.0).collect();
        assert_eq!(after_first, [1, 2].into());
        assert_eq!(mix[3], TrackId(100));
        let after_second: HashSet<i64> = mix[4..6].iter().map(|t| t.0).collect();
        assert_eq!(after_second, [101, 102].into());
    }

    #[test]
    fn rediscover_is_empty_without_history() {
        let m = model();
        let snap = Snapshot::new(vec![meta(1, 1, 1), meta(2, 2, 2)]);
        assert!(rediscover(&m, &snap, 100 * DAY_MS, true).is_empty());
    }
}
