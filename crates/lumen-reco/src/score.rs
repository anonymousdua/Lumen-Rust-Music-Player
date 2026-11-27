//! The scoring formula, its weights, and the diversity rules.
//!
//! ```text
//! score = 0.55 · audio_similarity   1 − normalized bliss distance
//!       + 0.20 · coplay             normalized co-play count
//!       + 0.15 · affinity           play count and completion rate, log-scaled
//!       + 0.05 · same artist or album
//!       − 0.25 · skip_rate          only with "Count skips as dislikes"
//!       − recency_penalty           strong if played in the last ~2 hours
//! ```

/// Every tunable number in one place.
#[derive(Clone, Debug, PartialEq)]
pub struct Weights {
    pub audio: f32,
    pub coplay: f32,
    pub affinity: f32,
    pub same_artist_or_album: f32,
    pub skip: f32,
    /// Penalty for a track played within `recent_ms` (outweighs any score).
    pub recent: f32,
    pub recent_ms: i64,
    /// After `recent_ms`, the penalty starts at `fading` and falls to zero
    /// at `fade_ms`.
    pub fading: f32,
    pub fade_ms: i64,
    /// Affinity: share given to play count (the rest is completion rate).
    pub affinity_plays_share: f32,
    /// Late-night mix: bonus for calm (slow, quiet) tracks.
    pub calm: f32,
    /// Diversity: at most `per_artist` tracks by one artist in any
    /// `artist_window` consecutive recommendations.
    pub per_artist: usize,
    pub artist_window: usize,
}

impl Default for Weights {
    fn default() -> Self {
        const HOUR: i64 = 3_600_000;
        Self {
            audio: 0.55,
            coplay: 0.20,
            affinity: 0.15,
            same_artist_or_album: 0.05,
            skip: 0.25,
            recent: 1.0,
            recent_ms: 2 * HOUR,
            fading: 0.15,
            fade_ms: 24 * HOUR,
            affinity_plays_share: 0.7,
            calm: 0.25,
            per_artist: 2,
            artist_window: 10,
        }
    }
}

/// A candidate's normalized signals, each 0 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScoreInputs {
    pub audio_similarity: f32,
    pub coplay: f32,
    pub affinity: f32,
    pub same_artist_or_album: bool,
    pub skip_rate: f32,
    pub recency_penalty: f32,
    /// Late-night mix only.
    pub calmness: Option<f32>,
}

pub fn score(w: &Weights, i: &ScoreInputs, skips_as_dislikes: bool) -> f32 {
    let mut s = w.audio * i.audio_similarity
        + w.coplay * i.coplay
        + w.affinity * i.affinity
        + if i.same_artist_or_album {
            w.same_artist_or_album
        } else {
            0.0
        }
        - i.recency_penalty;
    if skips_as_dislikes {
        s -= w.skip * i.skip_rate;
    }
    if let Some(c) = i.calmness {
        s += w.calm * c;
    }
    s
}

/// 1 − distance / the largest distance among the candidates.
pub fn audio_similarity(distance: f32, max_distance: f32) -> f32 {
    if max_distance <= 0.0 {
        1.0
    } else {
        (1.0 - distance / max_distance).clamp(0.0, 1.0)
    }
}

/// Log-scaled count, 0 to 1, against the largest count.
pub fn log_scaled(count: f32, max: f32) -> f32 {
    if max <= 0.0 {
        0.0
    } else {
        ((1.0 + count).ln() / (1.0 + max).ln()).clamp(0.0, 1.0)
    }
}

/// Play count (log-scaled against the most-played track) and completion rate.
pub fn affinity(w: &Weights, play_count: u32, max_play_count: u32, completion_rate: f32) -> f32 {
    let plays = log_scaled(play_count as f32, max_play_count as f32);
    w.affinity_plays_share * plays + (1.0 - w.affinity_plays_share) * completion_rate
}

/// Strong for 2 hours after a play, then fading to nothing over a day.
pub fn recency_penalty(w: &Weights, last_played_ms: Option<i64>, now_ms: i64) -> f32 {
    let Some(last) = last_played_ms else {
        return 0.0;
    };
    let age = now_ms - last;
    if age < w.recent_ms {
        w.recent
    } else if age < w.fade_ms {
        let span = (w.fade_ms - w.recent_ms) as f32;
        w.fading * (1.0 - (age - w.recent_ms) as f32 / span)
    } else {
        0.0
    }
}

/// Deterministic tie-break key from a seed (splitmix64).
pub fn tie_key(seed: u64, id: i64) -> u64 {
    let mut z = seed ^ (id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Builds a list under the diversity rules: at most `per_artist` tracks by
/// one artist in any `artist_window` in a row, and never the same album
/// twice in a row unless the seed came from that album. A track counts for
/// every artist credited on it ("A, B" and "A feat. C" are both by A).
pub struct Picker<'a> {
    w: &'a Weights,
    seed_albums: &'a [u32],
    /// (artists, album) of each pick so far.
    picked: Vec<(Vec<u32>, Option<u32>)>,
}

impl<'a> Picker<'a> {
    pub fn new(w: &'a Weights, seed_albums: &'a [u32]) -> Self {
        Self {
            w,
            seed_albums,
            picked: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.picked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    pub fn allows(&self, artists: &[u32], album: Option<u32>) -> bool {
        let window = self.w.artist_window.saturating_sub(1);
        let start = self.picked.len().saturating_sub(window);
        for artist in artists {
            let same = self.picked[start..]
                .iter()
                .filter(|p| p.0.contains(artist))
                .count();
            if same >= self.w.per_artist {
                return false;
            }
        }
        if let (Some(album), Some(prev)) = (album, self.picked.last())
            && prev.1 == Some(album)
            && !self.seed_albums.contains(&album)
        {
            return false;
        }
        true
    }

    pub fn push(&mut self, artists: &[u32], album: Option<u32>) {
        self.picked.push((artists.to_vec(), album));
    }

    /// Pushes if allowed.
    pub fn try_push(&mut self, artists: &[u32], album: Option<u32>) -> bool {
        let ok = self.allows(artists, album);
        if ok {
            self.push(artists, album);
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    #[test]
    fn formula_uses_the_weights() {
        let w = Weights::default();
        let i = ScoreInputs {
            audio_similarity: 1.0,
            coplay: 1.0,
            affinity: 1.0,
            same_artist_or_album: true,
            skip_rate: 1.0,
            recency_penalty: 0.0,
            calmness: None,
        };
        assert!((score(&w, &i, false) - 0.95).abs() < 1e-6);
        assert!((score(&w, &i, true) - 0.70).abs() < 1e-6);
        let only_audio = ScoreInputs {
            audio_similarity: 0.5,
            ..Default::default()
        };
        assert!((score(&w, &only_audio, true) - 0.275).abs() < 1e-6);
        let calm = ScoreInputs {
            calmness: Some(1.0),
            ..Default::default()
        };
        assert!((score(&w, &calm, true) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn recent_plays_sink_below_everything() {
        let w = Weights::default();
        let best = ScoreInputs {
            audio_similarity: 1.0,
            coplay: 1.0,
            affinity: 1.0,
            same_artist_or_album: true,
            recency_penalty: recency_penalty(&w, Some(0), HOUR),
            ..Default::default()
        };
        let worst = ScoreInputs::default();
        assert!(score(&w, &best, false) < score(&w, &worst, false));
    }

    #[test]
    fn recency_penalty_curve() {
        let w = Weights::default();
        assert_eq!(recency_penalty(&w, None, 10 * HOUR), 0.0);
        assert_eq!(recency_penalty(&w, Some(0), 0), 1.0);
        assert_eq!(recency_penalty(&w, Some(0), 2 * HOUR - 1), 1.0);
        assert!((recency_penalty(&w, Some(0), 2 * HOUR) - 0.15).abs() < 1e-6);
        assert!((recency_penalty(&w, Some(0), 13 * HOUR) - 0.075).abs() < 1e-6);
        assert_eq!(recency_penalty(&w, Some(0), 24 * HOUR), 0.0);
        assert_eq!(recency_penalty(&w, Some(0), 100 * HOUR), 0.0);
    }

    #[test]
    fn normalizations() {
        assert_eq!(audio_similarity(0.0, 4.0), 1.0);
        assert_eq!(audio_similarity(4.0, 4.0), 0.0);
        assert_eq!(audio_similarity(1.0, 4.0), 0.75);
        assert_eq!(audio_similarity(1.0, 0.0), 1.0);
        assert_eq!(log_scaled(0.0, 10.0), 0.0);
        assert_eq!(log_scaled(10.0, 10.0), 1.0);
        assert_eq!(log_scaled(3.0, 0.0), 0.0);
        assert!(
            log_scaled(3.0, 15.0) > 3.0 / 15.0,
            "log scale lifts small counts"
        );
        let w = Weights::default();
        assert!((affinity(&w, 10, 10, 1.0) - 1.0).abs() < 1e-6);
        assert!((affinity(&w, 0, 10, 0.5) - 0.15).abs() < 1e-6);
    }

    #[test]
    fn at_most_two_per_artist_in_ten() {
        let w = Weights::default();
        let mut p = Picker::new(&w, &[]);
        assert!(p.try_push(&[1], Some(10)));
        // A collaboration counts for each artist on it.
        assert!(p.try_push(&[7, 1], Some(11)));
        assert!(!p.allows(&[1], Some(12)), "third in the window");
        assert!(!p.allows(&[5, 1], Some(12)), "third, as a featured artist");
        for k in 0..7 {
            assert!(p.try_push(&[100 + k], None));
        }
        // 9 picks so far; the next window still holds both artist-1 picks.
        assert!(!p.allows(&[1], Some(12)));
        assert!(p.try_push(&[200], None));
        // The first artist-1 pick has left the window.
        assert!(p.allows(&[1], Some(12)));
        assert!(p.allows(&[], None), "unknown artist is unlimited");
    }

    #[test]
    fn no_album_back_to_back_unless_seeded() {
        let w = Weights::default();
        let mut p = Picker::new(&w, &[]);
        p.push(&[1], Some(10));
        assert!(!p.allows(&[2], Some(10)));
        assert!(p.allows(&[2], Some(11)));
        p.push(&[2], Some(11));
        assert!(p.allows(&[3], Some(10)), "not back-to-back any more");

        let seeds = [10];
        let mut p = Picker::new(&w, &seeds);
        p.push(&[1], Some(10));
        assert!(p.allows(&[2], Some(10)), "the seed's album may repeat");
    }

    #[test]
    fn tie_keys_are_stable_and_spread() {
        assert_eq!(tie_key(7, 42), tie_key(7, 42));
        assert_ne!(tie_key(7, 42), tie_key(8, 42));
        assert_ne!(tie_key(7, 42), tie_key(7, 43));
    }
}
