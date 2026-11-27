//! An in-memory snapshot of the library for the Songs, Albums and Artists
//! screens: tracks with their collation keys, plus albums and artists with
//! their totals. Sorting, filtering and search run here, without SQL, so a
//! new sort or a keystroke in the search box costs a few milliseconds even at
//! 10,000 tracks.
//!
//! Build one with [`crate::Reader::catalog`].

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use lumen_core::{AlbumSort, ArtistSort, SongFilter, SongSort, SortDir, Track, TrackId};

use crate::collate::Collation;

pub(crate) struct RawTrack {
    pub track: Track,
    pub album_id: i64,
    pub artist_id: i64,
    pub sort_title: Vec<u8>,
    pub sort_artist: Vec<u8>,
    pub sort_album: Vec<u8>,
}

pub(crate) struct RawAlbum {
    pub id: i64,
    pub title: String,
    pub album_artist: String,
    pub year: Option<i32>,
    pub cover_hash: Option<String>,
    pub sort_key: Vec<u8>,
}

pub(crate) struct RawArtist {
    pub id: i64,
    pub name: String,
    pub sort_key: Vec<u8>,
    pub added_ms: i64,
}

/// A track plus what sorting and search need.
pub struct CatTrack {
    pub track: Track,
    /// Index into [`Catalog::albums`].
    pub album: Option<usize>,
    /// Index into [`Catalog::artists`].
    pub artist: Option<usize>,
    sort_title: Box<[u8]>,
    sort_artist: Box<[u8]>,
    sort_album: Box<[u8]>,
    /// [`fold`]ed "title artist album" for search.
    search: Box<str>,
}

pub struct Album {
    pub id: i64,
    pub title: String,
    /// The album artist tag, else the track artist when every track shares
    /// it, else "Various artists".
    pub artist: String,
    pub year: Option<i32>,
    pub cover_hash: Option<String>,
    pub track_count: usize,
    pub duration_ms: u64,
    /// Earliest "date added" of its tracks.
    pub added_ms: i64,
    /// Newest file "date modified" of its tracks.
    pub modified_ms: i64,
    /// Track indices in disc, track number, title order.
    pub tracks: Vec<usize>,
    sort_key: Box<[u8]>,
    sort_artist: Box<[u8]>,
    search: Box<str>,
}

pub struct Artist {
    pub id: i64,
    pub name: String,
    pub track_count: usize,
    /// Album indices, newest release first.
    pub albums: Vec<usize>,
    pub plays: u64,
    pub last_played_ms: Option<i64>,
    pub added_ms: i64,
    /// Track indices in album, disc, track order.
    pub tracks: Vec<usize>,
    sort_key: Box<[u8]>,
    search: Box<str>,
}

/// Library totals for a list of tracks (the Songs subline).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListStats {
    pub tracks: u64,
    pub bytes: u64,
    pub duration_ms: u64,
    pub with_synced_lyrics: u64,
}

#[derive(Default)]
pub struct Catalog {
    pub tracks: Vec<CatTrack>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    by_id: HashMap<TrackId, usize>,
}

impl Catalog {
    pub(crate) fn build(
        raw: Vec<RawTrack>,
        mut albums: Vec<RawAlbum>,
        mut artists: Vec<RawArtist>,
    ) -> Self {
        let collation = Collation::new();
        // An album or artist row can briefly outlive its tracks between two
        // commits of a scan: leave those out.
        let used_albums: std::collections::HashSet<i64> = raw.iter().map(|r| r.album_id).collect();
        let used_artists: std::collections::HashSet<i64> =
            raw.iter().map(|r| r.artist_id).collect();
        albums.retain(|a| used_albums.contains(&a.id));
        artists.retain(|a| used_artists.contains(&a.id));
        let album_ix: HashMap<i64, usize> =
            albums.iter().enumerate().map(|(i, a)| (a.id, i)).collect();
        let artist_ix: HashMap<i64, usize> =
            artists.iter().enumerate().map(|(i, a)| (a.id, i)).collect();

        let tracks: Vec<CatTrack> = raw
            .into_iter()
            .map(|r| {
                let search = fold(&format!(
                    "{}\u{1}{}\u{1}{}\u{1}{}",
                    r.track.title,
                    r.track.artist,
                    r.track.album,
                    r.track.album_artist.as_deref().unwrap_or("")
                ));
                CatTrack {
                    album: album_ix.get(&r.album_id).copied(),
                    artist: artist_ix.get(&r.artist_id).copied(),
                    sort_title: r.sort_title.into(),
                    sort_artist: r.sort_artist.into(),
                    sort_album: r.sort_album.into(),
                    search: search.into(),
                    track: r.track,
                }
            })
            .collect();
        let by_id = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.track.id, i))
            .collect();

        // Group tracks by album and artist.
        let mut album_tracks: Vec<Vec<usize>> = vec![Vec::new(); albums.len()];
        let mut artist_tracks: Vec<Vec<usize>> = vec![Vec::new(); artists.len()];
        for (i, t) in tracks.iter().enumerate() {
            if let Some(a) = t.album {
                album_tracks[a].push(i);
            }
            if let Some(a) = t.artist {
                artist_tracks[a].push(i);
            }
        }

        let in_album_order = |a: &usize, b: &usize| {
            let (x, y) = (&tracks[*a], &tracks[*b]);
            x.track
                .disc_no
                .cmp(&y.track.disc_no)
                .then(x.track.track_no.cmp(&y.track.track_no))
                .then(x.sort_title.cmp(&y.sort_title))
                .then(x.track.id.cmp(&y.track.id))
        };

        let albums: Vec<Album> = albums
            .into_iter()
            .zip(album_tracks)
            .map(|(a, mut list)| {
                list.sort_by(in_album_order);
                let artist = if !a.album_artist.is_empty() {
                    a.album_artist.clone()
                } else {
                    let first = list.first().map(|&i| tracks[i].track.artist.as_str());
                    match first {
                        Some(f) if list.iter().all(|&i| tracks[i].track.artist == f) => {
                            f.to_string()
                        }
                        Some(_) => "Various artists".to_string(),
                        None => String::new(),
                    }
                };
                let cover_hash = a.cover_hash.clone().or_else(|| {
                    list.iter()
                        .find_map(|&i| tracks[i].track.cover_hash.clone())
                });
                let year = a
                    .year
                    .or_else(|| list.iter().find_map(|&i| tracks[i].track.year));
                Album {
                    id: a.id,
                    search: fold(&format!("{}\u{1}{}", a.title, artist)).into(),
                    sort_artist: collation.key(&artist).into(),
                    sort_key: a.sort_key.into(),
                    title: a.title,
                    artist,
                    year,
                    cover_hash,
                    track_count: list.len(),
                    duration_ms: list.iter().map(|&i| tracks[i].track.duration_ms).sum(),
                    added_ms: list
                        .iter()
                        .map(|&i| tracks[i].track.date_added_ms)
                        .min()
                        .unwrap_or(0),
                    modified_ms: list
                        .iter()
                        .map(|&i| tracks[i].track.mtime_ms)
                        .max()
                        .unwrap_or(0),
                    tracks: list,
                }
            })
            .collect();

        let artists: Vec<Artist> = artists
            .into_iter()
            .zip(artist_tracks)
            .map(|(a, mut list)| {
                list.sort_by(|x, y| {
                    let (p, q) = (&tracks[*x], &tracks[*y]);
                    p.sort_album
                        .cmp(&q.sort_album)
                        .then_with(|| in_album_order(x, y))
                });
                let mut album_list: Vec<usize> =
                    list.iter().filter_map(|&i| tracks[i].album).collect();
                album_list.sort_unstable();
                album_list.dedup();
                album_list.sort_by(|x, y| {
                    let (p, q) = (&albums[*x], &albums[*y]);
                    cmp_opt_last(p.year, q.year, SortDir::Desc).then(p.sort_key.cmp(&q.sort_key))
                });
                Artist {
                    id: a.id,
                    search: fold(&a.name).into(),
                    name: a.name,
                    sort_key: a.sort_key.into(),
                    track_count: list.len(),
                    albums: album_list,
                    plays: list
                        .iter()
                        .map(|&i| u64::from(tracks[i].track.play_count))
                        .sum(),
                    last_played_ms: list
                        .iter()
                        .filter_map(|&i| tracks[i].track.last_played_ms)
                        .max(),
                    added_ms: a.added_ms,
                    tracks: list,
                }
            })
            .collect();

        Self {
            tracks,
            albums,
            artists,
            by_id,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn track(&self, i: usize) -> &Track {
        &self.tracks[i].track
    }

    pub fn index_of(&self, id: TrackId) -> Option<usize> {
        self.by_id.get(&id).copied()
    }

    /// Tracks for the Songs screen: filtered by the chip and the search query,
    /// in the chosen order.
    pub fn songs(
        &self,
        sort: SongSort,
        dir: SortDir,
        filter: SongFilter,
        query: &str,
    ) -> Vec<usize> {
        let terms = terms(query);
        let mut out: Vec<usize> = (0..self.tracks.len())
            .filter(|&i| {
                let t = &self.tracks[i];
                filter.matches(&t.track) && matches_terms(&t.search, &terms)
            })
            .collect();
        self.sort_songs(&mut out, sort, dir);
        out
    }

    /// Sorts track indices. Ties fall back to artist, album, disc, track and
    /// title, ascending (the same order the database uses).
    pub fn sort_songs(&self, list: &mut [usize], sort: SongSort, dir: SortDir) {
        let tie = |a: &CatTrack, b: &CatTrack| {
            a.sort_artist
                .cmp(&b.sort_artist)
                .then_with(|| a.sort_album.cmp(&b.sort_album))
                .then(a.track.disc_no.cmp(&b.track.disc_no))
                .then(a.track.track_no.cmp(&b.track.track_no))
                .then_with(|| a.sort_title.cmp(&b.sort_title))
                .then(a.track.id.cmp(&b.track.id))
        };
        let primary = |a: &CatTrack, b: &CatTrack| -> Ordering {
            match sort {
                SongSort::Title => a.sort_title.cmp(&b.sort_title),
                SongSort::Artist => a.sort_artist.cmp(&b.sort_artist),
                SongSort::Album => a.sort_album.cmp(&b.sort_album),
                SongSort::Duration => a.track.duration_ms.cmp(&b.track.duration_ms),
                SongSort::DateModified => a.track.mtime_ms.cmp(&b.track.mtime_ms),
                SongSort::DateAdded => a.track.date_added_ms.cmp(&b.track.date_added_ms),
                SongSort::PlayCount => a.track.play_count.cmp(&b.track.play_count),
            }
        };
        // Sorting by album keeps each album in disc and track order.
        let album_tie = |a: &CatTrack, b: &CatTrack| {
            a.track
                .disc_no
                .cmp(&b.track.disc_no)
                .then(a.track.track_no.cmp(&b.track.track_no))
                .then_with(|| a.sort_title.cmp(&b.sort_title))
                .then(a.track.id.cmp(&b.track.id))
        };
        list.sort_by(|&x, &y| {
            let (a, b) = (&self.tracks[x], &self.tracks[y]);
            let o = directed(primary(a, b), dir);
            if sort == SongSort::Album {
                o.then_with(|| album_tie(a, b))
            } else {
                o.then_with(|| tie(a, b))
            }
        });
    }

    /// Albums for the Albums screen, filtered by title or artist.
    pub fn album_list(&self, sort: AlbumSort, dir: SortDir, query: &str) -> Vec<usize> {
        let terms = terms(query);
        let mut out: Vec<usize> = (0..self.albums.len())
            .filter(|&i| matches_terms(&self.albums[i].search, &terms))
            .collect();
        self.sort_albums(&mut out, sort, dir);
        out
    }

    pub fn sort_albums(&self, list: &mut [usize], sort: AlbumSort, dir: SortDir) {
        list.sort_by(|&x, &y| {
            let (a, b) = (&self.albums[x], &self.albums[y]);
            let primary = match sort {
                AlbumSort::Title => directed(a.sort_key.cmp(&b.sort_key), dir),
                AlbumSort::Artist => directed(a.sort_artist.cmp(&b.sort_artist), dir)
                    .then(cmp_opt_last(a.year, b.year, SortDir::Asc)),
                AlbumSort::Year => cmp_opt_last(a.year, b.year, dir),
                AlbumSort::DateAdded => directed(a.added_ms.cmp(&b.added_ms), dir),
                AlbumSort::DateModified => directed(a.modified_ms.cmp(&b.modified_ms), dir),
                AlbumSort::TrackCount => directed(a.track_count.cmp(&b.track_count), dir),
            };
            primary
                .then_with(|| a.sort_key.cmp(&b.sort_key))
                .then_with(|| a.sort_artist.cmp(&b.sort_artist))
                .then(a.id.cmp(&b.id))
        });
    }

    /// Artists for the Artists screen, filtered by name.
    pub fn artist_list(&self, sort: ArtistSort, dir: SortDir, query: &str) -> Vec<usize> {
        let terms = terms(query);
        let mut out: Vec<usize> = (0..self.artists.len())
            .filter(|&i| matches_terms(&self.artists[i].search, &terms))
            .collect();
        out.sort_by(|&x, &y| {
            let (a, b) = (&self.artists[x], &self.artists[y]);
            let primary = match sort {
                ArtistSort::Name => directed(a.sort_key.cmp(&b.sort_key), dir),
                ArtistSort::TrackCount => directed(a.track_count.cmp(&b.track_count), dir),
                ArtistSort::AlbumCount => directed(a.albums.len().cmp(&b.albums.len()), dir),
                ArtistSort::TotalPlays => directed(a.plays.cmp(&b.plays), dir),
                ArtistSort::RecentlyPlayed => cmp_opt_last(a.last_played_ms, b.last_played_ms, dir),
                ArtistSort::DateAdded => directed(a.added_ms.cmp(&b.added_ms), dir),
            };
            primary
                .then_with(|| a.sort_key.cmp(&b.sort_key))
                .then(a.id.cmp(&b.id))
        });
        out
    }

    /// Totals for a list of track indices.
    pub fn stats(&self, list: &[usize]) -> ListStats {
        let mut s = ListStats::default();
        for &i in list {
            let t = &self.tracks[i].track;
            s.tracks += 1;
            s.bytes += t.size;
            s.duration_ms += t.duration_ms;
            if t.lyrics.synced() {
                s.with_synced_lyrics += 1;
            }
        }
        s
    }

    /// Track ids for a list of indices (to build a play queue).
    pub fn ids(&self, list: &[usize]) -> Vec<TrackId> {
        list.iter().map(|&i| self.tracks[i].track.id).collect()
    }
}

impl Catalog {
    /// Sorts playlist rows `(catalog index, date added to playlist, position
    /// in the playlist)`. Custom order is the given order (reversed for
    /// descending); other sorts are stable, so ties keep the playlist's order.
    pub fn sort_playlist(
        &self,
        rows: &mut [(usize, i64, usize)],
        sort: lumen_core::PlaylistSort,
        dir: SortDir,
    ) {
        use lumen_core::PlaylistSort as P;
        match sort {
            P::Custom => {
                if dir == SortDir::Desc {
                    rows.reverse();
                }
            }
            P::Title => rows.sort_by(|a, b| {
                directed(
                    self.tracks[a.0]
                        .sort_title
                        .cmp(&self.tracks[b.0].sort_title),
                    dir,
                )
            }),
            P::Artist => rows.sort_by(|a, b| {
                directed(
                    self.tracks[a.0]
                        .sort_artist
                        .cmp(&self.tracks[b.0].sort_artist),
                    dir,
                )
            }),
            P::DateAdded => rows.sort_by(|a, b| directed(a.1.cmp(&b.1), dir)),
            P::DateModified => rows.sort_by(|a, b| {
                directed(self.track(a.0).mtime_ms.cmp(&self.track(b.0).mtime_ms), dir)
            }),
            P::Duration => rows.sort_by(|a, b| {
                directed(
                    self.track(a.0)
                        .duration_ms
                        .cmp(&self.track(b.0).duration_ms),
                    dir,
                )
            }),
        }
    }
}

/// A catalog of bare tracks (no albums or artists), for tests.
#[cfg(test)]
pub(crate) fn test_catalog(tracks: Vec<Track>) -> Catalog {
    let c = Collation::new();
    let raw = tracks
        .into_iter()
        .map(|t| RawTrack {
            sort_title: c.key(&t.title),
            sort_artist: c.key(&t.artist),
            sort_album: c.key(&t.album),
            album_id: 0,
            artist_id: 0,
            track: t,
        })
        .collect();
    Catalog::build(raw, Vec::new(), Vec::new())
}

/// Shared catalogs are swapped whole when the library changes.
pub type SharedCatalog = Arc<Catalog>;

/// Folds text for search: lowercase, and Latin accents removed ("Ñandú" →
/// "nandu"). Devanagari and Malayalam vowel signs are kept; only the Latin
/// combining accents (U+0300–U+036F) are dropped after NFD decomposition.
pub fn fold(s: &str) -> String {
    let nfd = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(s);
    nfd.chars()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .flat_map(char::to_lowercase)
        .collect()
}

fn terms(query: &str) -> Vec<String> {
    fold(query).split_whitespace().map(str::to_owned).collect()
}

fn matches_terms(haystack: &str, terms: &[String]) -> bool {
    terms.iter().all(|t| haystack.contains(t.as_str()))
}

fn directed(o: Ordering, dir: SortDir) -> Ordering {
    match dir {
        SortDir::Asc => o,
        SortDir::Desc => o.reverse(),
    }
}

/// Compares optional values in `dir`, with missing values last either way.
fn cmp_opt_last<T: Ord>(a: Option<T>, b: Option<T>, dir: SortDir) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => directed(x.cmp(&y), dir),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::LyricsFlags;

    fn track(id: i64, title: &str, artist: &str, album: &str, no: u32) -> RawTrack {
        let c = Collation::new();
        RawTrack {
            track: Track {
                id: TrackId(id),
                path: format!(r"D:\m\{id}.flac").into(),
                title: title.into(),
                artist: artist.into(),
                album_artist: None,
                album: album.into(),
                track_no: Some(no),
                disc_no: None,
                year: Some(2000 + id as i32),
                genre: None,
                duration_ms: 1000 * id as u64,
                sample_rate: Some(44_100),
                bit_depth: Some(16),
                channels: Some(2),
                codec: if id % 2 == 0 { "FLAC" } else { "MP3" }.into(),
                size: 100,
                mtime_ms: 10 - id,
                date_added_ms: id,
                cover_hash: None,
                lyrics: LyricsFlags(if id == 1 {
                    LyricsFlags::LRC | LyricsFlags::LRC_SYNCED
                } else {
                    0
                }),
                lyrics_offset_ms: 0,
                play_count: id as u32,
                last_played_ms: None,
            },
            album_id: if album == "Uno" { 1 } else { 2 },
            artist_id: if artist == "Rosalía" { 1 } else { 2 },
            sort_title: c.key(title),
            sort_artist: c.key(artist),
            sort_album: c.key(album),
        }
    }

    fn sample() -> Catalog {
        let c = Collation::new();
        Catalog::build(
            vec![
                track(1, "Ñandú", "Rosalía", "Uno", 2),
                track(2, "nube", "Rosalía", "Uno", 1),
                track(3, "Track 10", "अरिजीत", "Dos", 1),
                track(4, "Track 2", "अरिजीत", "Dos", 2),
                track(5, "മഴ", "Rosalía", "Dos", 3),
            ],
            vec![
                RawAlbum {
                    id: 1,
                    title: "Uno".into(),
                    album_artist: String::new(),
                    year: Some(2001),
                    cover_hash: None,
                    sort_key: c.key("Uno"),
                },
                RawAlbum {
                    id: 2,
                    title: "Dos".into(),
                    album_artist: String::new(),
                    year: None,
                    cover_hash: None,
                    sort_key: c.key("Dos"),
                },
            ],
            vec![
                RawArtist {
                    id: 1,
                    name: "Rosalía".into(),
                    sort_key: c.key("Rosalía"),
                    added_ms: 1,
                },
                RawArtist {
                    id: 2,
                    name: "अरिजीत".into(),
                    sort_key: c.key("अरिजीत"),
                    added_ms: 3,
                },
                // No tracks left: dropped.
                RawArtist {
                    id: 3,
                    name: "Gone".into(),
                    sort_key: c.key("Gone"),
                    added_ms: 0,
                },
            ],
        )
    }

    fn titles(c: &Catalog, list: &[usize]) -> Vec<String> {
        list.iter().map(|&i| c.track(i).title.clone()).collect()
    }

    #[test]
    fn songs_sort_filter_and_search() {
        let c = sample();
        let all = c.songs(SongSort::Title, SortDir::Asc, SongFilter::All, "");
        assert_eq!(
            titles(&c, &all),
            ["nube", "Ñandú", "Track 2", "Track 10", "മഴ"]
        );
        let desc = c.songs(SongSort::Title, SortDir::Desc, SongFilter::All, "");
        assert_eq!(
            titles(&c, &desc),
            ["മഴ", "Track 10", "Track 2", "Ñandú", "nube"]
        );
        let plays = c.songs(SongSort::PlayCount, SortDir::Desc, SongFilter::All, "");
        assert_eq!(c.track(plays[0]).title, "മഴ");
        let flac = c.songs(SongSort::Title, SortDir::Asc, SongFilter::FlacOnly, "");
        assert_eq!(titles(&c, &flac), ["nube", "Track 2"]);
        let synced = c.songs(SongSort::Title, SortDir::Asc, SongFilter::SyncedLyrics, "");
        assert_eq!(titles(&c, &synced), ["Ñandú"]);
        assert_eq!(
            c.songs(SongSort::Title, SortDir::Asc, SongFilter::MissingLyrics, "")
                .len(),
            4
        );
        // Accent-insensitive, every term must match, any field.
        let q = c.songs(SongSort::Title, SortDir::Asc, SongFilter::All, "nandu");
        assert_eq!(titles(&c, &q), ["Ñandú"]);
        let q = c.songs(
            SongSort::Title,
            SortDir::Asc,
            SongFilter::All,
            "ROSALIA uno",
        );
        assert_eq!(titles(&c, &q), ["nube", "Ñandú"]);
        let q = c.songs(SongSort::Title, SortDir::Asc, SongFilter::All, "अरिजीत");
        assert_eq!(q.len(), 2);
        let q = c.songs(SongSort::Title, SortDir::Asc, SongFilter::All, "മഴ");
        assert_eq!(titles(&c, &q), ["മഴ"]);
        let s = c.stats(&all);
        assert_eq!((s.tracks, s.bytes, s.with_synced_lyrics), (5, 500, 1));
    }

    #[test]
    fn albums_and_artists() {
        let c = sample();
        let uno = &c.albums[0];
        assert_eq!(uno.artist, "Rosalía");
        assert_eq!(titles(&c, &uno.tracks), ["nube", "Ñandú"]);
        let dos = &c.albums[1];
        assert_eq!(dos.artist, "Various artists");
        assert_eq!(dos.year, Some(2003), "falls back to a track year");
        assert_eq!(dos.track_count, 3);
        assert_eq!(dos.duration_ms, 12_000);

        let by_title = c.album_list(AlbumSort::Title, SortDir::Asc, "");
        assert_eq!(by_title, [1, 0]);
        let by_count = c.album_list(AlbumSort::TrackCount, SortDir::Desc, "");
        assert_eq!(by_count, [1, 0]);
        assert_eq!(c.album_list(AlbumSort::Title, SortDir::Asc, "various"), [1]);

        assert_eq!(c.artists.len(), 2, "artist without tracks is dropped");
        let names = |l: Vec<usize>| -> Vec<String> {
            l.into_iter().map(|i| c.artists[i].name.clone()).collect()
        };
        assert_eq!(
            names(c.artist_list(ArtistSort::Name, SortDir::Asc, "")),
            ["Rosalía", "अरिजीत"]
        );
        assert_eq!(
            names(c.artist_list(ArtistSort::TrackCount, SortDir::Desc, "")),
            ["Rosalía", "अरिजीत"]
        );
        assert_eq!(
            names(c.artist_list(ArtistSort::Name, SortDir::Asc, "rosa")),
            ["Rosalía"]
        );
        let rosalia = &c.artists[0];
        assert_eq!(rosalia.albums.len(), 2);
        assert_eq!(rosalia.plays, 1 + 2 + 5);
        // Every track links to a live artist index.
        for t in &c.tracks {
            let a = t.artist.expect("artist");
            assert_eq!(c.artists[a].name, t.track.artist);
        }
    }

    #[test]
    fn playlist_sorts() {
        use lumen_core::PlaylistSort as P;
        let c = sample();
        // Playlist order: Track 10, Ñandú, nube (with added times).
        let custom = vec![(2usize, 30i64, 0usize), (0, 10, 1), (1, 20, 2)];
        let order = |sort, dir| {
            let mut r = custom.clone();
            c.sort_playlist(&mut r, sort, dir);
            r.into_iter()
                .map(|(i, _, _)| c.track(i).title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(P::Custom, SortDir::Asc),
            ["Track 10", "Ñandú", "nube"]
        );
        assert_eq!(
            order(P::Custom, SortDir::Desc),
            ["nube", "Ñandú", "Track 10"]
        );
        assert_eq!(order(P::Title, SortDir::Asc), ["nube", "Ñandú", "Track 10"]);
        assert_eq!(
            order(P::DateAdded, SortDir::Desc),
            ["Track 10", "nube", "Ñandú"]
        );
        assert_eq!(
            order(P::Duration, SortDir::Desc),
            ["Track 10", "nube", "Ñandú"]
        );
        assert_eq!(order(P::Artist, SortDir::Asc)[2], "Track 10");
    }

    #[test]
    fn folding() {
        assert_eq!(fold("Ñandú Árbol"), "nandu arbol");
        assert_eq!(fold("CORAZÓN"), "corazon");
        // Indic vowel signs stay.
        assert_eq!(fold("चाँद"), "चाँद");
        assert_eq!(fold("നിലാവ്"), "നിലാവ്");
    }
}
