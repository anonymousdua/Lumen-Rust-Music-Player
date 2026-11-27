//! Listening signals: play events, sessions, skips and the co-play matrix,
//! kept in memory (loaded at startup, updated as plays come in) and in the
//! database (`store.rs`).

use std::collections::HashMap;

use lumen_core::TrackId;

/// A new session starts after this much silence.
pub const SESSION_GAP_MS: i64 = 30 * 60 * 1000;
/// Moving on before this much listening is a skip.
pub const SKIP_WITHIN_MS: u64 = 30_000;
/// Listening to this share of a track counts as finishing it.
const COMPLETE_SHARE: f64 = 0.9;
/// Tracks remembered per session for co-play counts.
const SESSION_MEMORY: usize = 200;

/// One finished play, as the player reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayRecord {
    pub track: TrackId,
    /// When playback of it started (ms since epoch).
    pub started_at: i64,
    /// When it stopped: another track started, the queue ended, or Lumen quit.
    pub ended_at: i64,
    /// Time actually heard (seeking doesn't count).
    pub listened_ms: u64,
    pub duration_ms: u64,
    /// It played to its end (rather than the listener moving on).
    pub reached_end: bool,
    /// The listener stopped it by quitting Lumen (never a skip).
    pub interrupted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    /// Moved on within 30 seconds.
    Skipped,
    /// Heard part of it.
    Partial,
}

/// Completed, skipped or partly heard.
pub fn classify(
    listened_ms: u64,
    duration_ms: u64,
    reached_end: bool,
    interrupted: bool,
) -> Outcome {
    let share_done = duration_ms > 0 && listened_ms as f64 >= COMPLETE_SHARE * duration_ms as f64;
    if reached_end || share_done {
        Outcome::Completed
    } else if listened_ms < SKIP_WITHIN_MS && !interrupted {
        Outcome::Skipped
    } else {
        Outcome::Partial
    }
}

/// The session a play starting at `started_at` belongs to, given the last
/// play's session and end time. A session id is the start of its first play.
pub fn session_for(last: Option<(i64, i64)>, started_at: i64) -> i64 {
    match last {
        Some((session, ended_at)) if started_at - ended_at <= SESSION_GAP_MS => session,
        _ => started_at,
    }
}

/// "After 9 pm": 21:00 to 03:59 local time.
pub fn is_late_night(local_hour: u8) -> bool {
    !(4..21).contains(&local_hour)
}

/// Totals for one track over its play events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackStats {
    pub events: u32,
    pub completions: u32,
    pub skips: u32,
    /// Plays (not skips) that started late at night.
    pub late_night: u32,
    pub last_ended_ms: i64,
}

impl TrackStats {
    pub fn completion_rate(&self) -> f32 {
        if self.events == 0 {
            0.0
        } else {
            self.completions as f32 / self.events as f32
        }
    }

    /// Skips over events, smoothed so one early skip isn't a verdict.
    pub fn skip_rate(&self) -> f32 {
        self.skips as f32 / (self.events as f32 + 1.0)
    }
}

/// What a recorded play changes in the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub session_id: i64,
    pub outcome: Outcome,
    /// Co-play pairs `(a, b)`, `a < b`, each one more time.
    pub pairs: Vec<(i64, i64)>,
}

#[derive(Default)]
pub struct Signals {
    pub stats: HashMap<TrackId, TrackStats>,
    /// Symmetric co-play counts: `coplay[a][b] == coplay[b][a]`.
    coplay: HashMap<i64, HashMap<i64, u32>>,
    /// Current session and when its last play ended.
    session: Option<(i64, i64)>,
    /// Tracks heard (not skipped) in the current session, oldest first.
    session_tracks: Vec<i64>,
}

impl Signals {
    pub fn coplay(&self, a: TrackId, b: TrackId) -> u32 {
        self.coplay
            .get(&a.0)
            .and_then(|m| m.get(&b.0))
            .copied()
            .unwrap_or(0)
    }

    /// Tracks played in the same session as `a`, with counts.
    pub fn coplayed_with(&self, a: TrackId) -> impl Iterator<Item = (TrackId, u32)> + '_ {
        self.coplay
            .get(&a.0)
            .into_iter()
            .flat_map(|m| m.iter().map(|(&b, &n)| (TrackId(b), n)))
    }

    pub fn add_coplay(&mut self, a: i64, b: i64, n: u32) {
        if a == b {
            return;
        }
        *self.coplay.entry(a).or_default().entry(b).or_default() += n;
        *self.coplay.entry(b).or_default().entry(a).or_default() += n;
    }

    /// Restores the session in progress (startup).
    pub fn resume_session(&mut self, session: i64, last_ended: i64, heard: Vec<i64>) {
        self.session = Some((session, last_ended));
        self.session_tracks = heard;
    }

    pub fn add_stats(&mut self, id: TrackId, s: TrackStats) {
        self.stats.insert(id, s);
    }

    /// Records a play: its session, outcome, the track's totals and the
    /// co-play pairs it makes with what this session already played.
    pub fn record(&mut self, r: &PlayRecord, local_hour: u8) -> Applied {
        let outcome = classify(r.listened_ms, r.duration_ms, r.reached_end, r.interrupted);
        let session_id = session_for(self.session, r.started_at);
        if self.session.map(|s| s.0) != Some(session_id) {
            self.session_tracks.clear();
        }
        self.session = Some((session_id, r.ended_at));

        let st = self.stats.entry(r.track).or_default();
        st.events += 1;
        st.last_ended_ms = st.last_ended_ms.max(r.ended_at);
        match outcome {
            Outcome::Completed => st.completions += 1,
            Outcome::Skipped => st.skips += 1,
            Outcome::Partial => {}
        }
        if outcome != Outcome::Skipped && is_late_night(local_hour) {
            st.late_night += 1;
        }

        let mut pairs = Vec::new();
        if outcome != Outcome::Skipped && !self.session_tracks.contains(&r.track.0) {
            for &other in &self.session_tracks {
                pairs.push((other.min(r.track.0), other.max(r.track.0)));
            }
            for &(a, b) in &pairs {
                self.add_coplay(a, b, 1);
            }
            self.session_tracks.push(r.track.0);
            if self.session_tracks.len() > SESSION_MEMORY {
                self.session_tracks.remove(0);
            }
        }
        Applied {
            session_id,
            outcome,
            pairs,
        }
    }

    /// "Reset listening history".
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn rec(id: i64, start_min: i64, listened_s: u64, end: bool) -> PlayRecord {
        PlayRecord {
            track: TrackId(id),
            started_at: start_min * MIN,
            ended_at: start_min * MIN + listened_s as i64 * 1000,
            listened_ms: listened_s * 1000,
            duration_ms: 200_000,
            reached_end: end,
            interrupted: false,
        }
    }

    #[test]
    fn skip_detection() {
        assert_eq!(classify(10_000, 200_000, false, false), Outcome::Skipped);
        assert_eq!(classify(29_999, 200_000, false, false), Outcome::Skipped);
        assert_eq!(classify(30_000, 200_000, false, false), Outcome::Partial);
        assert_eq!(classify(185_000, 200_000, false, false), Outcome::Completed);
        assert_eq!(classify(50_000, 200_000, true, false), Outcome::Completed);
        // A short track played through is never a skip.
        assert_eq!(classify(20_000, 20_000, true, false), Outcome::Completed);
        // Quitting Lumen early isn't a judgement of the song.
        assert_eq!(classify(5_000, 200_000, false, true), Outcome::Partial);
    }

    #[test]
    fn sessions_split_after_30_minutes() {
        assert_eq!(session_for(None, 100), 100);
        let last = Some((1_000, 10 * MIN));
        assert_eq!(session_for(last, 10 * MIN + 30 * MIN), 1_000);
        assert_eq!(
            session_for(last, 10 * MIN + 30 * MIN + 1),
            10 * MIN + 30 * MIN + 1
        );
    }

    #[test]
    fn records_sessions_stats_and_coplay() {
        let mut s = Signals::default();
        let a = s.record(&rec(1, 0, 200, true), 22);
        assert_eq!(a.session_id, 0);
        assert!(a.pairs.is_empty());
        let b = s.record(&rec(2, 4, 100, false), 22);
        assert_eq!((b.session_id, b.outcome), (0, Outcome::Partial));
        assert_eq!(b.pairs, [(1, 2)]);
        // Skipped: counted as a skip, no co-play.
        let c = s.record(&rec(3, 6, 10, false), 22);
        assert_eq!(c.outcome, Outcome::Skipped);
        assert!(c.pairs.is_empty());
        let d = s.record(&rec(4, 7, 200, true), 12);
        assert_eq!(d.pairs, [(1, 4), (2, 4)]);
        assert_eq!(s.coplay(TrackId(4), TrackId(1)), 1);
        assert_eq!(s.coplay(TrackId(3), TrackId(1)), 0);

        // An hour later: a new session, no pairs with the old one.
        let e = s.record(&rec(1, 120, 200, true), 12);
        assert_eq!(e.session_id, 120 * MIN);
        assert!(e.pairs.is_empty());

        let st = s.stats[&TrackId(1)];
        assert_eq!(
            (st.events, st.completions, st.skips, st.late_night),
            (2, 2, 0, 1)
        );
        assert_eq!(s.stats[&TrackId(3)].skips, 1);
        assert_eq!(
            s.stats[&TrackId(3)].late_night,
            0,
            "skips aren't late-night plays"
        );
        assert!((s.stats[&TrackId(3)].skip_rate() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn replaying_a_track_in_a_session_adds_no_pairs() {
        let mut s = Signals::default();
        s.record(&rec(1, 0, 200, true), 10);
        s.record(&rec(2, 4, 200, true), 10);
        let again = s.record(&rec(1, 8, 200, true), 10);
        assert!(again.pairs.is_empty());
        assert_eq!(s.coplay(TrackId(1), TrackId(2)), 1);
    }

    #[test]
    fn late_night_hours() {
        assert!(is_late_night(21) && is_late_night(23) && is_late_night(0) && is_late_night(3));
        assert!(!is_late_night(4) && !is_late_night(12) && !is_late_night(20));
    }
}
