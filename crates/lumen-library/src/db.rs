//! SQLite access. [`Db`] is the single writer (owned by the library thread);
//! [`Reader`] is a read-only connection for the UI (WAL lets both run at once).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lumen_core::{LyricsFlags, SortDir, Track, TrackId};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

use crate::collate::{COLLATION_VERSION, Collation};
use crate::paths::is_within;
use crate::read::ScannedTrack;
use crate::schema;

pub type DbResult<T> = Result<T, String>;

fn e(err: rusqlite::Error) -> String {
    format!("database error: {err}")
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A library folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: i64,
    pub path: PathBuf,
    pub added_at: i64,
}

/// A folder with its track count (Settings › Library).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderInfo {
    pub id: i64,
    pub path: PathBuf,
    pub tracks: u64,
    /// False when the folder is missing (drive unplugged, deleted).
    pub available: bool,
}

/// Totals for the sidebar status box and the Songs header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryStats {
    pub tracks: u64,
    pub albums: u64,
    pub artists: u64,
    pub total_bytes: u64,
    pub total_duration_ms: u64,
    pub with_synced_lyrics: u64,
    pub folders: u64,
    /// When the last full scan finished (ms since epoch).
    pub last_scan_ms: Option<i64>,
}

/// One entry of a user playlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaylistEntry {
    pub track: TrackId,
    /// When it was added to the playlist.
    pub added_ms: i64,
}

/// `library.db` → `library.not-lumen-<n>.db`, the first name that is free.
fn set_aside_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "library".into());
    let dir = path.parent().unwrap_or(Path::new(""));
    (1..)
        .map(|n| dir.join(format!("{stem}.not-lumen-{n}.db")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join(format!("{stem}.not-lumen.db")))
}

/// A user playlist (for menus and the Playlists screen).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistInfo {
    pub id: i64,
    pub name: String,
    pub tracks: u64,
}

/// What the scanner needs to know about a stored track.
#[derive(Clone, Copy, Debug)]
pub struct Existing {
    pub id: i64,
    pub size: u64,
    pub mtime_ms: i64,
    pub lrc_mtime: Option<i64>,
    pub lyrics: u8,
}

/// Sort orders for track lists (the same enum the Songs screen uses).
pub use lumen_core::SongSort as TrackSort;

fn order_by(sort: TrackSort, dir: SortDir) -> String {
    let d = match dir {
        SortDir::Asc => "ASC",
        SortDir::Desc => "DESC",
    };
    // Ties fall back to artist, album, disc, track, title in ascending order.
    let tie = "sort_artist, sort_album, disc_no, track_no, sort_title, id";
    match sort {
        TrackSort::Title => format!("sort_title {d}, {tie}"),
        TrackSort::Artist => {
            format!("sort_artist {d}, sort_album, disc_no, track_no, sort_title, id")
        }
        TrackSort::Album => format!("sort_album {d}, disc_no, track_no, sort_title, id"),
        TrackSort::Duration => format!("duration_ms {d}, {tie}"),
        TrackSort::DateModified => format!("COALESCE(date_modified, mtime) {d}, {tie}"),
        TrackSort::DateAdded => format!("date_added {d}, {tie}"),
        TrackSort::PlayCount => format!("play_count {d}, {tie}"),
    }
}

const TRACK_COLUMNS: &str = "id, path, title, artist, album_artist, album, track_no, disc_no, \
    year, genre, duration_ms, sample_rate, bit_depth, channels, codec, size, \
    COALESCE(date_modified, mtime), date_added, \
    cover_hash, lyrics, lyrics_offset_ms, play_count, last_played";

fn row_to_track(r: &rusqlite::Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
        id: TrackId(r.get(0)?),
        path: PathBuf::from(r.get::<_, String>(1)?),
        title: r.get(2)?,
        artist: r.get(3)?,
        album_artist: r.get(4)?,
        album: r.get(5)?,
        track_no: r.get(6)?,
        disc_no: r.get(7)?,
        year: r.get(8)?,
        genre: r.get(9)?,
        duration_ms: r.get::<_, i64>(10)? as u64,
        sample_rate: r.get(11)?,
        bit_depth: r.get(12)?,
        channels: r.get(13)?,
        codec: r.get(14)?,
        size: r.get::<_, i64>(15)? as u64,
        mtime_ms: r.get(16)?,
        date_added_ms: r.get(17)?,
        cover_hash: r.get(18)?,
        lyrics: LyricsFlags(r.get::<_, i64>(19)? as u8),
        lyrics_offset_ms: r.get(20)?,
        play_count: r.get::<_, i64>(21)? as u32,
        last_played_ms: r.get(22)?,
    })
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The writer connection.
pub struct Db {
    /// Set when a database from another program was found at this path and
    /// moved aside (kept, renamed) so a new library could be created.
    pub set_aside: Option<PathBuf>,
    conn: Connection,
    collation: Collation,
    /// Update "Date modified" when a file changes ("Track file date
    /// modified"). Off keeps the value from when the file was first seen.
    pub track_mtime: bool,
}

impl Db {
    pub fn open(path: &Path) -> DbResult<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
        }
        let mut set_aside = None;
        let mut conn = Connection::open(path).map_err(e)?;
        if !schema::is_ours(&conn).map_err(e)? {
            // Not a Lumen library (another program, or an old prototype):
            // keep it, renamed, and start a new library in its place.
            drop(conn);
            let aside = set_aside_name(path);
            for suffix in ["", "-wal", "-shm"] {
                let from = PathBuf::from(format!("{}{suffix}", path.display()));
                if from.exists() {
                    let to = PathBuf::from(format!("{}{suffix}", aside.display()));
                    std::fs::rename(&from, &to).map_err(|err| {
                        format!(
                            "{} is not a Lumen library and could not be moved aside: {err}",
                            path.display()
                        )
                    })?;
                }
            }
            tracing::warn!(
                "{} was not a Lumen library; moved it to {} and started a new one",
                path.display(),
                aside.display()
            );
            set_aside = Some(aside);
            conn = Connection::open(path).map_err(e)?;
        }
        schema::configure(&conn).map_err(e)?;
        schema::migrate(&mut conn).map_err(e)?;
        let mut db = Self {
            set_aside,
            conn,
            collation: Collation::new(),
            track_mtime: true,
        };
        db.check_collation()?;
        Ok(db)
    }

    /// Rebuilds stored sort keys when the collation changed.
    fn check_collation(&mut self) -> DbResult<()> {
        let stored = self.meta("collation")?;
        if stored.as_deref() == Some(COLLATION_VERSION) {
            return Ok(());
        }
        let tx = self.conn.transaction().map_err(e)?;
        {
            let c = &self.collation;
            let rekey = |table: &str, cols: &[(&str, &str)]| -> DbResult<()> {
                let select = format!(
                    "SELECT id, {} FROM {table}",
                    cols.iter()
                        .map(|(src, _)| *src)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let rows: Vec<(i64, Vec<String>)> = {
                    let mut st = tx.prepare(&select).map_err(e)?;
                    st.query_map([], |r| {
                        let mut vals = Vec::new();
                        for i in 0..cols.len() {
                            vals.push(r.get::<_, String>(i + 1)?);
                        }
                        Ok((r.get(0)?, vals))
                    })
                    .map_err(e)?
                    .collect::<Result<_, _>>()
                    .map_err(e)?
                };
                for (id, vals) in rows {
                    for ((_, dst), v) in cols.iter().zip(vals) {
                        tx.execute(
                            &format!("UPDATE {table} SET {dst} = ?1 WHERE id = ?2"),
                            params![c.key(&v), id],
                        )
                        .map_err(e)?;
                    }
                }
                Ok(())
            };
            rekey(
                "tracks",
                &[
                    ("title", "sort_title"),
                    ("artist", "sort_artist"),
                    ("album", "sort_album"),
                ],
            )?;
            rekey("albums", &[("title", "sort_key")])?;
            rekey("artists", &[("name", "sort_key")])?;
        }
        tx.execute(
            "INSERT INTO meta(key, value) VALUES('collation', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [COLLATION_VERSION],
        )
        .map_err(e)?;
        tx.commit().map_err(e)
    }

    pub fn meta(&self, key: &str) -> DbResult<Option<String>> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(e)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> DbResult<()> {
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, value],
            )
            .map(|_| ())
            .map_err(e)
    }

    // ---- folders ----------------------------------------------------------

    pub fn folders(&self) -> DbResult<Vec<Folder>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT id, path, added_at FROM folders ORDER BY path")
            .map_err(e)?;
        st.query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                added_at: r.get(2)?,
            })
        })
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)
    }

    /// Adds a folder. A folder inside an existing one is rejected; an existing
    /// folder inside the new one is merged into it (its tracks are kept).
    pub fn add_folder(&mut self, path: &Path) -> DbResult<i64> {
        let existing = self.folders()?;
        if let Some(parent) = existing.iter().find(|f| is_within(path, &f.path)) {
            return Err(format!(
                "{} is already in the library (inside {})",
                path.display(),
                parent.path.display()
            ));
        }
        let tx = self.conn.transaction().map_err(e)?;
        let id: i64 = tx
            .query_row(
                "INSERT INTO folders(path, added_at) VALUES(?1, ?2) RETURNING id",
                params![path_str(path), now_ms()],
                |r| r.get(0),
            )
            .map_err(e)?;
        for child in existing.iter().filter(|f| is_within(&f.path, path)) {
            tx.execute(
                "UPDATE tracks SET folder_id = ?1 WHERE folder_id = ?2",
                params![id, child.id],
            )
            .map_err(e)?;
            tx.execute("DELETE FROM folders WHERE id = ?1", [child.id])
                .map_err(e)?;
        }
        tx.commit().map_err(e)?;
        Ok(id)
    }

    /// Removes a folder and its tracks from the library (files are untouched).
    pub fn remove_folder(&mut self, id: i64) -> DbResult<()> {
        let tx = self.conn.transaction().map_err(e)?;
        tx.execute("DELETE FROM tracks WHERE folder_id = ?1", [id])
            .map_err(e)?;
        tx.execute("DELETE FROM folders WHERE id = ?1", [id])
            .map_err(e)?;
        prune(&tx)?;
        tx.commit().map_err(e)
    }

    // ---- scanning ----------------------------------------------------------

    /// Stored tracks of a folder, keyed by lowercase path.
    pub fn existing_in_folder(&self, folder_id: i64) -> DbResult<HashMap<String, Existing>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT id, path, size, mtime, lrc_mtime, lyrics FROM tracks WHERE folder_id = ?1",
            )
            .map_err(e)?;
        let rows = st
            .query_map([folder_id], |r| {
                Ok((
                    r.get::<_, String>(1)?.to_lowercase(),
                    Existing {
                        id: r.get(0)?,
                        size: r.get::<_, i64>(2)? as u64,
                        mtime_ms: r.get(3)?,
                        lrc_mtime: r.get(4)?,
                        lyrics: r.get::<_, i64>(5)? as u8,
                    },
                ))
            })
            .map_err(e)?;
        rows.collect::<Result<_, _>>().map_err(e)
    }

    pub fn existing(&self, path: &Path) -> DbResult<Option<Existing>> {
        self.conn
            .query_row(
                "SELECT id, size, mtime, lrc_mtime, lyrics FROM tracks WHERE path = ?1",
                [path_str(path)],
                |r| {
                    Ok(Existing {
                        id: r.get(0)?,
                        size: r.get::<_, i64>(1)? as u64,
                        mtime_ms: r.get(2)?,
                        lrc_mtime: r.get(3)?,
                        lyrics: r.get::<_, i64>(4)? as u8,
                    })
                },
            )
            .optional()
            .map_err(e)
    }

    /// Track ids and paths under `dir` (any folder).
    pub fn tracks_under(&self, folder_id: i64, dir: &Path) -> DbResult<Vec<(i64, PathBuf)>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT id, path FROM tracks WHERE folder_id = ?1")
            .map_err(e)?;
        let rows: Vec<(i64, PathBuf)> = st
            .query_map([folder_id], |r| {
                Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?)))
            })
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        Ok(rows
            .into_iter()
            .filter(|(_, p)| is_within(p, dir))
            .collect())
    }

    /// Writes scanned tracks in one transaction. Returns (added, updated).
    pub fn write_tracks(
        &mut self,
        folder_id: i64,
        tracks: &[ScannedTrack],
    ) -> DbResult<(usize, usize)> {
        let now = now_ms();
        let tx = self.conn.transaction().map_err(e)?;
        let (mut added, mut updated) = (0, 0);
        for t in tracks {
            if upsert_track(&tx, &self.collation, folder_id, t, now, self.track_mtime)? {
                added += 1;
            } else {
                updated += 1;
            }
        }
        tx.commit().map_err(e)?;
        Ok((added, updated))
    }

    /// A file moved or was renamed: keep its id, history and playlists.
    pub fn move_track(&mut self, id: i64, to: &Path, folder_id: i64) -> DbResult<()> {
        self.conn
            .execute(
                "UPDATE tracks SET path = ?1, folder_id = ?2 WHERE id = ?3",
                params![path_str(to), folder_id, id],
            )
            .map(|_| ())
            .map_err(e)
    }

    pub fn update_lyrics(&self, id: i64, flags: u8, lrc_mtime: Option<i64>) -> DbResult<()> {
        self.conn
            .execute(
                "UPDATE tracks SET lyrics = ?1, lrc_mtime = ?2 WHERE id = ?3",
                params![i64::from(flags), lrc_mtime, id],
            )
            .map(|_| ())
            .map_err(e)
    }

    pub fn delete_tracks(&mut self, ids: &[i64]) -> DbResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction().map_err(e)?;
        {
            let mut st = tx
                .prepare_cached("DELETE FROM tracks WHERE id = ?1")
                .map_err(e)?;
            for id in ids {
                st.execute([id]).map_err(e)?;
            }
        }
        prune(&tx)?;
        tx.commit().map_err(e)?;
        Ok(ids.len())
    }

    // ---- listening data ----------------------------------------------------

    pub fn record_play(&self, id: TrackId) -> DbResult<()> {
        self.conn
            .execute(
                "UPDATE tracks SET play_count = play_count + 1, last_played = ?1 WHERE id = ?2",
                params![now_ms(), id.0],
            )
            .map(|_| ())
            .map_err(e)
    }

    pub fn reset_play_counts(&self) -> DbResult<()> {
        self.conn
            .execute("UPDATE tracks SET play_count = 0, last_played = NULL", [])
            .map(|_| ())
            .map_err(e)
    }

    pub fn set_lyrics_offset(&self, id: TrackId, offset_ms: i32) -> DbResult<()> {
        self.conn
            .execute(
                "UPDATE tracks SET lyrics_offset_ms = ?1 WHERE id = ?2",
                params![offset_ms, id.0],
            )
            .map(|_| ())
            .map_err(e)
    }

    // ---- playlists ---------------------------------------------------------

    pub fn create_playlist(&self, name: &str) -> DbResult<i64> {
        let now = now_ms();
        self.conn
            .query_row(
                "INSERT INTO playlists(name, created_at, updated_at) VALUES(?1, ?2, ?2) RETURNING id",
                params![name, now],
                |r| r.get(0),
            )
            .map_err(e)
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> DbResult<()> {
        self.conn
            .execute(
                "UPDATE playlists SET name = ?1, updated_at = ?2 WHERE id = ?3",
                params![name, now_ms(), id],
            )
            .map(|_| ())
            .map_err(e)
    }

    pub fn delete_playlist(&self, id: i64) -> DbResult<()> {
        self.conn
            .execute("DELETE FROM playlists WHERE id = ?1", [id])
            .map(|_| ())
            .map_err(e)
    }

    pub fn add_to_playlist(&mut self, id: i64, tracks: &[TrackId]) -> DbResult<()> {
        let now = now_ms();
        let tx = self.conn.transaction().map_err(e)?;
        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_tracks WHERE playlist_id = ?1",
                [id],
                |r| r.get(0),
            )
            .map_err(e)?;
        for (i, t) in tracks.iter().enumerate() {
            tx.execute(
                "INSERT INTO playlist_tracks(playlist_id, position, track_id, added_at) VALUES(?1, ?2, ?3, ?4)",
                params![id, next + i as i64, t.0, now],
            )
            .map_err(e)?;
        }
        tx.execute(
            "UPDATE playlists SET updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )
        .map_err(e)?;
        tx.commit().map_err(e)
    }
}

impl Db {
    /// Rewrites a playlist's order (positions 0..n) from `(track, added_at)`.
    fn rewrite_playlist(&mut self, id: i64, rows: &[(i64, i64)]) -> DbResult<()> {
        let tx = self.conn.transaction().map_err(e)?;
        tx.execute("DELETE FROM playlist_tracks WHERE playlist_id = ?1", [id])
            .map_err(e)?;
        for (i, (track, added)) in rows.iter().enumerate() {
            tx.execute(
                "INSERT INTO playlist_tracks(playlist_id, position, track_id, added_at) VALUES(?1, ?2, ?3, ?4)",
                params![id, i as i64, track, added],
            )
            .map_err(e)?;
        }
        tx.execute(
            "UPDATE playlists SET updated_at = ?1 WHERE id = ?2",
            params![now_ms(), id],
        )
        .map_err(e)?;
        tx.commit().map_err(e)
    }

    fn playlist_rows(&self, id: i64) -> DbResult<Vec<(i64, i64)>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT track_id, added_at FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
            )
            .map_err(e)?;
        st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)
    }

    /// Moves the entry at `from` so it ends up at index `to` (drag to reorder).
    pub fn move_in_playlist(&mut self, id: i64, from: usize, to: usize) -> DbResult<()> {
        let mut rows = self.playlist_rows(id)?;
        if from >= rows.len() || from == to {
            return Ok(());
        }
        let row = rows.remove(from);
        rows.insert(to.min(rows.len()), row);
        self.rewrite_playlist(id, &rows)
    }

    /// Removes the entry at `index`.
    pub fn remove_from_playlist(&mut self, id: i64, index: usize) -> DbResult<()> {
        let mut rows = self.playlist_rows(id)?;
        if index < rows.len() {
            rows.remove(index);
            self.rewrite_playlist(id, &rows)?;
        }
        Ok(())
    }

    /// Track id for a path (case-insensitive, as Windows paths are).
    pub fn track_id_by_path(&self, path: &Path) -> DbResult<Option<TrackId>> {
        self.conn
            .query_row(
                "SELECT id FROM tracks WHERE path = ?1",
                [path_str(path)],
                |r| Ok(TrackId(r.get(0)?)),
            )
            .optional()
            .map_err(e)
    }
}

/// Inserts or updates one track. Returns true when it was new.
fn upsert_track(
    tx: &Transaction<'_>,
    c: &Collation,
    folder_id: i64,
    t: &ScannedTrack,
    now: i64,
    track_mtime: bool,
) -> DbResult<bool> {
    let artist_id: i64 = tx
        .query_row(
            "INSERT INTO artists(name, sort_key, added_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET name = excluded.name RETURNING id",
            params![t.artist, c.key(&t.artist), now],
            |r| r.get(0),
        )
        .map_err(e)?;
    let album_artist = t.album_artist.clone().unwrap_or_default();
    // Without an album artist, the folder keeps same-named albums apart.
    let dir = if album_artist.is_empty() {
        t.path.parent().map(path_str).unwrap_or_default()
    } else {
        String::new()
    };
    let album_id: i64 = tx
        .query_row(
            "INSERT INTO albums(title, album_artist, dir, year, cover_hash, sort_key, added_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(title, album_artist, dir) DO UPDATE SET
                 year = COALESCE(albums.year, excluded.year),
                 cover_hash = COALESCE(excluded.cover_hash, albums.cover_hash)
             RETURNING id",
            params![
                t.album,
                album_artist,
                dir,
                t.year,
                t.cover_hash,
                c.key(&t.album),
                now
            ],
            |r| r.get(0),
        )
        .map_err(e)?;
    let existed: bool = tx
        .query_row(
            "SELECT 1 FROM tracks WHERE path = ?1",
            [path_str(&t.path)],
            |_| Ok(true),
        )
        .optional()
        .map_err(e)?
        .unwrap_or(false);
    tx.execute(
        "INSERT INTO tracks(path, folder_id, size, mtime, date_added, title, artist, album_artist,
             album, artist_id, album_id, track_no, disc_no, year, genre, duration_ms, sample_rate,
             bit_depth, channels, bitrate, codec, cover_hash, lrc_mtime, lyrics,
             sort_title, sort_artist, sort_album, date_modified)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
             ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?4)
         ON CONFLICT(path) DO UPDATE SET
             date_modified = CASE WHEN ?28 THEN excluded.mtime
                                  ELSE COALESCE(tracks.date_modified, excluded.mtime) END,
             folder_id = excluded.folder_id, size = excluded.size, mtime = excluded.mtime,
             title = excluded.title, artist = excluded.artist, album_artist = excluded.album_artist,
             album = excluded.album, artist_id = excluded.artist_id, album_id = excluded.album_id,
             track_no = excluded.track_no, disc_no = excluded.disc_no, year = excluded.year,
             genre = excluded.genre, duration_ms = excluded.duration_ms,
             sample_rate = excluded.sample_rate, bit_depth = excluded.bit_depth,
             channels = excluded.channels, bitrate = excluded.bitrate, codec = excluded.codec,
             cover_hash = excluded.cover_hash, lrc_mtime = excluded.lrc_mtime,
             lyrics = excluded.lyrics, sort_title = excluded.sort_title,
             sort_artist = excluded.sort_artist, sort_album = excluded.sort_album",
        params![
            path_str(&t.path),
            folder_id,
            t.stat.size as i64,
            t.stat.mtime_ms,
            now,
            t.title,
            t.artist,
            t.album_artist,
            t.album,
            artist_id,
            album_id,
            t.track_no,
            t.disc_no,
            t.year,
            t.genre,
            t.duration_ms as i64,
            t.sample_rate,
            t.bit_depth,
            t.channels,
            t.bitrate,
            t.codec,
            t.cover_hash,
            t.lrc_mtime,
            i64::from(t.lyrics.0),
            c.key(&t.title),
            c.key(&t.artist),
            c.key(&t.album),
            track_mtime,
        ],
    )
    .map_err(e)?;
    Ok(!existed)
}

/// Drops albums and artists that no longer have tracks.
fn prune(tx: &Transaction<'_>) -> DbResult<()> {
    tx.execute_batch(
        "DELETE FROM albums WHERE id NOT IN (SELECT album_id FROM tracks WHERE album_id IS NOT NULL);
         DELETE FROM artists WHERE id NOT IN (SELECT artist_id FROM tracks WHERE artist_id IS NOT NULL);",
    )
    .map_err(e)
}

/// Read-only connection for the UI thread.
pub struct Reader {
    conn: Connection,
}

impl Reader {
    pub fn open(path: &Path) -> DbResult<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(e)?;
        conn.execute_batch("PRAGMA busy_timeout = 2000; PRAGMA query_only = ON;")
            .map_err(e)?;
        Ok(Self { conn })
    }

    pub fn stats(&self) -> DbResult<LibraryStats> {
        let (tracks, total_bytes, total_duration_ms, with_synced_lyrics) = self
            .conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0), COALESCE(SUM(duration_ms), 0),
                        COALESCE(SUM(CASE WHEN lyrics & ?1 != 0 THEN 1 ELSE 0 END), 0)
                 FROM tracks",
                [i64::from(
                    LyricsFlags::LRC_SYNCED | LyricsFlags::EMBEDDED_SYNCED,
                )],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .map_err(e)?;
        let count = |sql: &str| -> DbResult<u64> {
            self.conn
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .map(|v| v as u64)
                .map_err(e)
        };
        let last_scan_ms = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'last_scan'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map_err(e)?
            .and_then(|v| v.parse().ok());
        Ok(LibraryStats {
            tracks: tracks as u64,
            albums: count("SELECT COUNT(*) FROM albums")?,
            artists: count("SELECT COUNT(*) FROM artists")?,
            total_bytes: total_bytes as u64,
            total_duration_ms: total_duration_ms as u64,
            with_synced_lyrics: with_synced_lyrics as u64,
            folders: count("SELECT COUNT(*) FROM folders")?,
            last_scan_ms,
        })
    }

    pub fn folders(&self) -> DbResult<Vec<FolderInfo>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT f.id, f.path, (SELECT COUNT(*) FROM tracks t WHERE t.folder_id = f.id)
                 FROM folders f ORDER BY f.path",
            )
            .map_err(e)?;
        st.query_map([], |r| {
            let path = PathBuf::from(r.get::<_, String>(1)?);
            Ok(FolderInfo {
                id: r.get(0)?,
                available: path.is_dir(),
                path,
                tracks: r.get::<_, i64>(2)? as u64,
            })
        })
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)
    }

    pub fn tracks(&self, sort: TrackSort, dir: SortDir) -> DbResult<Vec<Track>> {
        let sql = format!(
            "SELECT {TRACK_COLUMNS} FROM tracks ORDER BY {}",
            order_by(sort, dir)
        );
        let mut st = self.conn.prepare_cached(&sql).map_err(e)?;
        st.query_map([], row_to_track)
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)
    }

    pub fn track(&self, id: TrackId) -> DbResult<Option<Track>> {
        let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE id = ?1");
        self.conn
            .query_row(&sql, [id.0], row_to_track)
            .optional()
            .map_err(e)
    }

    pub fn track_by_path(&self, path: &Path) -> DbResult<Option<Track>> {
        let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE path = ?1");
        self.conn
            .query_row(&sql, [path_str(path)], row_to_track)
            .optional()
            .map_err(e)
    }

    /// Album titles in collation order (used by tests and the Albums batch).
    pub fn album_titles(&self) -> DbResult<Vec<String>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT title FROM albums ORDER BY sort_key, id")
            .map_err(e)?;
        st.query_map([], |r| r.get(0))
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)
    }

    /// User playlists by name, with track counts.
    pub fn playlists(&self) -> DbResult<Vec<PlaylistInfo>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT p.id, p.name, (SELECT COUNT(*) FROM playlist_tracks t WHERE t.playlist_id = p.id)
                 FROM playlists p ORDER BY p.name COLLATE NOCASE, p.id",
            )
            .map_err(e)?;
        st.query_map([], |r| {
            Ok(PlaylistInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                tracks: r.get::<_, i64>(2)? as u64,
            })
        })
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)
    }

    /// Entries of a playlist in custom order: (track, date added).
    pub fn playlist_entries(&self, id: i64) -> DbResult<Vec<PlaylistEntry>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT track_id, added_at FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
            )
            .map_err(e)?;
        st.query_map([id], |r| {
            Ok(PlaylistEntry {
                track: TrackId(r.get(0)?),
                added_ms: r.get(1)?,
            })
        })
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)
    }

    /// Track ids of a playlist in custom order.
    pub fn playlist_track_ids(&self, id: i64) -> DbResult<Vec<TrackId>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT track_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
            )
            .map_err(e)?;
        st.query_map([id], |r| Ok(TrackId(r.get(0)?)))
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)
    }

    /// Everything the library screens show, read in one go (about 5 ms for
    /// 2,500 tracks). See [`crate::catalog::Catalog`].
    pub fn catalog(&self) -> DbResult<crate::catalog::Catalog> {
        use crate::catalog::{RawAlbum, RawArtist, RawTrack};
        let sql = format!(
            "SELECT {TRACK_COLUMNS}, album_id, artist_id, sort_title, sort_artist, sort_album
             FROM tracks"
        );
        let mut st = self.conn.prepare_cached(&sql).map_err(e)?;
        let tracks: Vec<RawTrack> = st
            .query_map([], |r| {
                Ok(RawTrack {
                    track: row_to_track(r)?,
                    album_id: r.get::<_, Option<i64>>(23)?.unwrap_or(0),
                    artist_id: r.get::<_, Option<i64>>(24)?.unwrap_or(0),
                    sort_title: r.get(25)?,
                    sort_artist: r.get(26)?,
                    sort_album: r.get(27)?,
                })
            })
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT id, title, album_artist, year, cover_hash, sort_key FROM albums",
            )
            .map_err(e)?;
        let albums: Vec<RawAlbum> = st
            .query_map([], |r| {
                Ok(RawAlbum {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    album_artist: r.get(2)?,
                    year: r.get(3)?,
                    cover_hash: r.get(4)?,
                    sort_key: r.get(5)?,
                })
            })
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        let mut st = self
            .conn
            .prepare_cached("SELECT id, name, sort_key, added_at FROM artists")
            .map_err(e)?;
        let artists: Vec<RawArtist> = st
            .query_map([], |r| {
                Ok(RawArtist {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    sort_key: r.get(2)?,
                    added_ms: r.get(3)?,
                })
            })
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        Ok(crate::catalog::Catalog::build(tracks, albums, artists))
    }

    /// Album cover hash, for tests and thumbnails.
    pub fn album_cover(&self, title: &str) -> DbResult<Option<String>> {
        self.conn
            .query_row(
                "SELECT cover_hash FROM albums WHERE title = ?1",
                [title],
                |r| r.get(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(e)
    }
}
