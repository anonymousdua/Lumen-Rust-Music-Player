//! Built-in, read-only smart playlists and their rules.

use lumen_core::{SongSort, SortDir, Track};

use crate::catalog::Catalog;

/// "Most played" keeps this many tracks.
pub const MOST_PLAYED_LIMIT: usize = 50;
/// "Recently added" looks back this far.
pub const RECENT_DAYS: i64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SmartPlaylist {
    HiRes,
    MostPlayed,
    RecentlyAdded,
    MissingLyrics,
}

impl SmartPlaylist {
    pub const ALL: [SmartPlaylist; 4] = [
        SmartPlaylist::HiRes,
        SmartPlaylist::MostPlayed,
        SmartPlaylist::RecentlyAdded,
        SmartPlaylist::MissingLyrics,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SmartPlaylist::HiRes => "Hi-res only",
            SmartPlaylist::MostPlayed => "Most played",
            SmartPlaylist::RecentlyAdded => "Recently added",
            SmartPlaylist::MissingLyrics => "Missing lyrics",
        }
    }

    /// Whether a single track qualifies (before Most played's top-50 cut).
    pub fn matches(self, t: &Track, now_ms: i64) -> bool {
        match self {
            // Bit depth ≥ 24 or rate > 48 kHz.
            SmartPlaylist::HiRes => {
                t.bit_depth.is_some_and(|b| b >= 24) || t.sample_rate.is_some_and(|r| r > 48_000)
            }
            SmartPlaylist::MostPlayed => t.play_count > 0,
            SmartPlaylist::RecentlyAdded => t.date_added_ms >= now_ms - RECENT_DAYS * 86_400_000,
            SmartPlaylist::MissingLyrics => !t.lyrics.any(),
        }
    }

    /// The playlist in its own ("custom") order, as catalog indices with the
    /// date each counts as added (the track's date added).
    pub fn rows(self, c: &Catalog, now_ms: i64) -> Vec<(usize, i64)> {
        let mut ix: Vec<usize> = (0..c.tracks.len())
            .filter(|&i| self.matches(c.track(i), now_ms))
            .collect();
        match self {
            SmartPlaylist::HiRes | SmartPlaylist::MissingLyrics => {
                c.sort_songs(&mut ix, SongSort::Artist, SortDir::Asc)
            }
            SmartPlaylist::MostPlayed => {
                ix.sort_by(|&a, &b| {
                    let (x, y) = (c.track(a), c.track(b));
                    y.play_count
                        .cmp(&x.play_count)
                        .then(y.last_played_ms.cmp(&x.last_played_ms))
                        .then(x.id.cmp(&y.id))
                });
                ix.truncate(MOST_PLAYED_LIMIT);
            }
            SmartPlaylist::RecentlyAdded => {
                c.sort_songs(&mut ix, SongSort::DateAdded, SortDir::Desc)
            }
        }
        ix.into_iter()
            .map(|i| (i, c.track(i).date_added_ms))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_catalog;
    use lumen_core::{LyricsFlags, TrackId};

    const NOW: i64 = 1_800_000_000_000;
    const DAY: i64 = 86_400_000;

    fn t(
        id: i64,
        bits: Option<u8>,
        rate: Option<u32>,
        plays: u32,
        added_days_ago: i64,
        lyrics: u8,
    ) -> Track {
        Track {
            id: TrackId(id),
            path: format!(r"D:\m\{id}.flac").into(),
            title: format!("T{id}"),
            artist: "A".into(),
            album_artist: None,
            album: "Al".into(),
            track_no: Some(id as u32),
            disc_no: None,
            year: None,
            genre: None,
            duration_ms: 1000,
            sample_rate: rate,
            bit_depth: bits,
            channels: Some(2),
            codec: "FLAC".into(),
            size: 1,
            mtime_ms: 0,
            date_added_ms: NOW - added_days_ago * DAY,
            cover_hash: None,
            lyrics: LyricsFlags(lyrics),
            lyrics_offset_ms: 0,
            play_count: plays,
            last_played_ms: (plays > 0).then_some(NOW - id),
        }
    }

    fn ids(c: &Catalog, rows: &[(usize, i64)]) -> Vec<i64> {
        rows.iter().map(|&(i, _)| c.track(i).id.0).collect()
    }

    #[test]
    fn rules() {
        let c = test_catalog(vec![
            t(1, Some(16), Some(44_100), 0, 100, 0),
            t(2, Some(24), Some(44_100), 5, 1, LyricsFlags::LRC),
            t(3, Some(16), Some(96_000), 9, 31, 0),
            t(4, None, Some(48_000), 5, 30, LyricsFlags::EMBEDDED),
            t(5, Some(16), Some(48_000), 0, 0, 0),
        ]);
        assert_eq!(ids(&c, &SmartPlaylist::HiRes.rows(&c, NOW)), [2, 3]);
        // Most played: plays desc, then most recently played.
        assert_eq!(ids(&c, &SmartPlaylist::MostPlayed.rows(&c, NOW)), [3, 2, 4]);
        // Recently added: last 30 days (exactly 30 counts), newest first.
        assert_eq!(
            ids(&c, &SmartPlaylist::RecentlyAdded.rows(&c, NOW)),
            [5, 2, 4]
        );
        assert_eq!(
            ids(&c, &SmartPlaylist::MissingLyrics.rows(&c, NOW)),
            [1, 3, 5]
        );
    }

    #[test]
    fn most_played_keeps_the_top_fifty() {
        let c = test_catalog(
            (1..=60)
                .map(|i| t(i, None, None, i as u32, 400, 0))
                .collect(),
        );
        let rows = SmartPlaylist::MostPlayed.rows(&c, NOW);
        assert_eq!(rows.len(), MOST_PLAYED_LIMIT);
        assert_eq!(ids(&c, &rows)[0], 60);
        assert_eq!(*ids(&c, &rows).last().unwrap_or(&0), 11);
    }
}
