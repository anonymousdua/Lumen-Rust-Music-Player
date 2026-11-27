//! Database access for recommendations: the `track_analysis`,
//! `play_events`, `coplay` and `mixes` tables (lumen-library migration v3),
//! through a connection of our own.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lumen_core::TrackId;
use rusqlite::{Connection, OptionalExtension, params};

use crate::features::{self, Vector};
use crate::signals::{Applied, Outcome, PlayRecord, Signals, TrackStats};

pub type StoreResult<T> = Result<T, String>;

fn e(err: rusqlite::Error) -> String {
    format!("recommendations database: {err}")
}

/// "Re-analyze library": anything analyzed before this time is redone.
const REANALYZE_KEY: &str = "reco.reanalyze_before";

pub struct Store {
    conn: Connection,
}

/// A track waiting for analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: TrackId,
    pub path: PathBuf,
    pub size: i64,
    pub mtime: i64,
}

impl Store {
    /// Opens the library database (already migrated by `lumen-library`).
    pub fn open(path: &Path) -> StoreResult<Self> {
        let conn = Connection::open(path).map_err(e)?;
        lumen_library::schema::configure(&conn).map_err(e)?;
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(e)?;
        if version < 3 {
            return Err("the library database has no recommendation tables".into());
        }
        Ok(Self { conn })
    }

    // ---- analysis ------------------------------------------------------------

    /// Every stored vector of the current feature version.
    pub fn vectors(&self) -> StoreResult<Vec<(TrackId, Vector)>> {
        let mut st = self
            .conn
            .prepare("SELECT track_id, features FROM track_analysis WHERE features_version = ?1")
            .map_err(e)?;
        let rows = st
            .query_map([features::features_version()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(e)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, blob) = row.map_err(e)?;
            if let Some(v) = features::from_blob(&blob) {
                out.push((TrackId(id), v));
            }
        }
        Ok(out)
    }

    /// Tracks with no current analysis: never analyzed, file changed (size
    /// or mtime), an older feature version, or analyzed before "Re-analyze
    /// library". In track id order.
    pub fn pending(&self) -> StoreResult<Vec<Pending>> {
        let before = self.reanalyze_before()?;
        let mut st = self
            .conn
            .prepare(
                "SELECT t.id, t.path, t.size, t.mtime FROM tracks t
                 LEFT JOIN track_analysis a ON a.track_id = t.id
                 WHERE a.track_id IS NULL
                    OR a.features_version != ?1
                    OR a.size != t.size
                    OR a.mtime != t.mtime
                    OR a.analyzed_at < ?2
                 ORDER BY t.id",
            )
            .map_err(e)?;
        let rows = st
            .query_map(params![features::features_version(), before], |r| {
                Ok(Pending {
                    id: TrackId(r.get(0)?),
                    path: PathBuf::from(r.get::<_, String>(1)?),
                    size: r.get(2)?,
                    mtime: r.get(3)?,
                })
            })
            .map_err(e)?;
        rows.collect::<Result<_, _>>().map_err(e)
    }

    /// (tracks analyzed and current, tracks in the library).
    pub fn analysis_counts(&self) -> StoreResult<(usize, usize)> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .map_err(e)?;
        let pending = self.pending()?.len() as i64;
        Ok(((total - pending).max(0) as usize, total as usize))
    }

    pub fn save_vector(&self, p: &Pending, v: &Vector, now_ms: i64) -> StoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO track_analysis
                   (track_id, features, features_version, size, mtime, analyzed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(track_id) DO UPDATE SET
                   features = excluded.features,
                   features_version = excluded.features_version,
                   size = excluded.size,
                   mtime = excluded.mtime,
                   analyzed_at = excluded.analyzed_at",
                params![
                    p.id.0,
                    features::to_blob(v),
                    features::features_version(),
                    p.size,
                    p.mtime,
                    now_ms
                ],
            )
            // The track was removed while it was being analyzed.
            .map(|_| ())
            .or_else(|err| match err {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    Ok(())
                }
                other => Err(e(other)),
            })
    }

    fn reanalyze_before(&self) -> StoreResult<i64> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [REANALYZE_KEY],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    /// "Re-analyze library": everything analyzed so far is redone. Old
    /// vectors stay usable until their replacement arrives.
    pub fn request_reanalysis(&self, now_ms: i64) -> StoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![REANALYZE_KEY, now_ms.to_string()],
            )
            .map(|_| ())
            .map_err(e)
    }

    // ---- listening history -----------------------------------------------------

    /// Totals per track, co-play counts and the session in progress.
    pub fn signals(&self) -> StoreResult<Signals> {
        let mut s = Signals::default();
        let mut st = self
            .conn
            .prepare(
                "SELECT track_id, COUNT(*), SUM(completed), SUM(skipped), MAX(ended_at),
                        -- Late-night plays: the hours of signals::is_late_night.
                        SUM(CASE WHEN skipped = 0 AND (local_hour >= 21 OR local_hour < 4)
                                 THEN 1 ELSE 0 END)
                 FROM play_events GROUP BY track_id",
            )
            .map_err(e)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    TrackId(r.get(0)?),
                    TrackStats {
                        events: r.get(1)?,
                        completions: r.get(2)?,
                        skips: r.get(3)?,
                        last_ended_ms: r.get(4)?,
                        late_night: r.get(5)?,
                    },
                ))
            })
            .map_err(e)?;
        for row in rows {
            let (id, stats) = row.map_err(e)?;
            s.add_stats(id, stats);
        }

        let mut st = self
            .conn
            .prepare("SELECT a, b, count FROM coplay")
            .map_err(e)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })
            .map_err(e)?;
        for row in rows {
            let (a, b, n) = row.map_err(e)?;
            s.add_coplay(a, b, n);
        }

        let last: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT session_id, ended_at FROM play_events ORDER BY ended_at DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?;
        if let Some((session, ended)) = last {
            let mut st = self
                .conn
                .prepare(
                    "SELECT track_id FROM play_events WHERE session_id = ?1 AND skipped = 0
                     GROUP BY track_id ORDER BY MIN(started_at)",
                )
                .map_err(e)?;
            let heard: Vec<i64> = st
                .query_map([session], |r| r.get(0))
                .map_err(e)?
                .collect::<Result<_, _>>()
                .map_err(e)?;
            s.resume_session(session, ended, heard);
        }
        Ok(s)
    }

    /// Writes one play and its co-play pairs (as [`Signals::record`] decided).
    pub fn save_play(
        &mut self,
        r: &PlayRecord,
        local_hour: u8,
        applied: &Applied,
    ) -> StoreResult<()> {
        let tx = self.conn.transaction().map_err(e)?;
        let inserted = tx.execute(
            "INSERT INTO play_events
               (track_id, started_at, ended_at, listened_ms, completed, skipped, session_id, local_hour)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                r.track.0,
                r.started_at,
                r.ended_at,
                r.listened_ms as i64,
                applied.outcome == Outcome::Completed,
                applied.outcome == Outcome::Skipped,
                applied.session_id,
                local_hour
            ],
        );
        match inserted {
            Ok(_) => {}
            // The track is gone from the library: nothing to remember.
            Err(rusqlite::Error::SqliteFailure(f, _))
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Ok(());
            }
            Err(err) => return Err(e(err)),
        }
        {
            let mut st = tx
                .prepare_cached(
                    "INSERT INTO coplay (a, b, count) VALUES (?1, ?2, 1)
                     ON CONFLICT(a, b) DO UPDATE SET count = count + 1",
                )
                .map_err(e)?;
            for &(a, b) in &applied.pairs {
                // A pair whose other track was removed is skipped.
                match st.execute(params![a, b]) {
                    Ok(_) => {}
                    Err(rusqlite::Error::SqliteFailure(f, _))
                        if f.code == rusqlite::ErrorCode::ConstraintViolation => {}
                    Err(err) => return Err(e(err)),
                }
            }
        }
        tx.commit().map_err(e)
    }

    /// "Reset listening history": play events, co-play counts and the
    /// saved mixes. The audio analysis stays.
    pub fn reset_history(&mut self) -> StoreResult<()> {
        let tx = self.conn.transaction().map_err(e)?;
        tx.execute_batch("DELETE FROM play_events; DELETE FROM coplay; DELETE FROM mixes;")
            .map_err(e)?;
        tx.commit().map_err(e)
    }

    pub fn play_event_count(&self) -> StoreResult<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM play_events", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as u64)
            .map_err(e)
    }

    // ---- mixes ---------------------------------------------------------------

    /// A saved mix: when it was made and its tracks.
    pub fn mix(&self, key: &str) -> StoreResult<Option<(i64, Vec<TrackId>)>> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT generated_at, tracks FROM mixes WHERE kind = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?;
        Ok(row.map(|(at, list)| {
            let ids = list
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .map(TrackId)
                .collect();
            (at, ids)
        }))
    }

    pub fn save_mix(&self, key: &str, generated_at: i64, tracks: &[TrackId]) -> StoreResult<()> {
        let list: Vec<String> = tracks.iter().map(|t| t.0.to_string()).collect();
        self.conn
            .execute(
                "INSERT INTO mixes (kind, generated_at, tracks) VALUES (?1, ?2, ?3)
                 ON CONFLICT(kind) DO UPDATE SET
                   generated_at = excluded.generated_at, tracks = excluded.tracks",
                params![key, generated_at, list.join(",")],
            )
            .map(|_| ())
            .map_err(e)
    }

    /// Track ids still in the library (to drop removed tracks from a mix).
    pub fn existing(&self, ids: &[TrackId]) -> StoreResult<HashSet<TrackId>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT 1 FROM tracks WHERE id = ?1")
            .map_err(e)?;
        let mut out = HashSet::new();
        for id in ids {
            if st.exists([id.0]).map_err(e)? {
                out.insert(*id);
            }
        }
        Ok(out)
    }
}
