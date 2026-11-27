//! Database schema and migrations (`PRAGMA user_version`).

use rusqlite::Connection;

/// Each entry migrates from version `i` to `i + 1`. Never edit a shipped
/// migration; append a new one.
const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE folders (
        id       INTEGER PRIMARY KEY,
        path     TEXT NOT NULL UNIQUE COLLATE NOCASE,
        added_at INTEGER NOT NULL
    );

    CREATE TABLE artists (
        id       INTEGER PRIMARY KEY,
        name     TEXT NOT NULL UNIQUE,
        sort_key BLOB NOT NULL,
        added_at INTEGER NOT NULL
    );

    -- An album is (title, album artist). Without an album artist tag, the
    -- folder tells compilations apart.
    CREATE TABLE albums (
        id           INTEGER PRIMARY KEY,
        title        TEXT NOT NULL,
        album_artist TEXT NOT NULL DEFAULT '',
        dir          TEXT NOT NULL DEFAULT '',
        year         INTEGER,
        cover_hash   TEXT,
        sort_key     BLOB NOT NULL,
        added_at     INTEGER NOT NULL,
        UNIQUE (title, album_artist, dir)
    );

    CREATE TABLE tracks (
        id               INTEGER PRIMARY KEY,
        path             TEXT NOT NULL UNIQUE COLLATE NOCASE,
        folder_id        INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        size             INTEGER NOT NULL,
        mtime            INTEGER NOT NULL,   -- ms since epoch ("Date modified")
        date_added       INTEGER NOT NULL,   -- ms, first time Lumen saw the file
        title            TEXT NOT NULL,
        artist           TEXT NOT NULL,
        album_artist     TEXT,
        album            TEXT NOT NULL,
        artist_id        INTEGER REFERENCES artists(id),
        album_id         INTEGER REFERENCES albums(id),
        track_no         INTEGER,
        disc_no          INTEGER,
        year             INTEGER,
        genre            TEXT,
        duration_ms      INTEGER NOT NULL,
        sample_rate      INTEGER,
        bit_depth        INTEGER,
        channels         INTEGER,
        bitrate          INTEGER,
        codec            TEXT NOT NULL,
        cover_hash       TEXT,
        lrc_mtime        INTEGER,            -- mtime of the sibling .lrc, if any
        lyrics           INTEGER NOT NULL DEFAULT 0,  -- LyricsFlags
        lyrics_offset_ms INTEGER NOT NULL DEFAULT 0,
        play_count       INTEGER NOT NULL DEFAULT 0,
        last_played      INTEGER,
        sort_title       BLOB NOT NULL,
        sort_artist      BLOB NOT NULL,
        sort_album       BLOB NOT NULL
    );
    CREATE INDEX tracks_folder ON tracks(folder_id);
    CREATE INDEX tracks_album  ON tracks(album_id);
    CREATE INDEX tracks_artist ON tracks(artist_id);
    CREATE INDEX tracks_sort_title ON tracks(sort_title);
    CREATE INDEX tracks_sort_artist ON tracks(sort_artist);
    CREATE INDEX tracks_mtime ON tracks(mtime);
    CREATE INDEX tracks_added ON tracks(date_added);
    CREATE INDEX tracks_size_mtime ON tracks(size, mtime);

    CREATE TABLE playlists (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE TABLE playlist_tracks (
        playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
        position    INTEGER NOT NULL,
        track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
        added_at    INTEGER NOT NULL,
        PRIMARY KEY (playlist_id, position)
    );
    CREATE INDEX playlist_tracks_track ON playlist_tracks(track_id);
    "#,
    // v2: "Date modified" as shown, separate from the file mtime the scanner
    // compares, so "Track file date modified" can be turned off.
    r#"
    ALTER TABLE tracks ADD COLUMN date_modified INTEGER;
    UPDATE tracks SET date_modified = mtime;
    "#,
];

pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// `PRAGMA application_id` of a Lumen library ("LUMN"), so a database from
/// another program is never mistaken for ours.
pub const APPLICATION_ID: i32 = 0x4C55_4D4E;

/// Whether `conn` holds a database Lumen can use: empty (new), stamped with
/// our application id, or an older Lumen library from before the stamp.
pub fn is_ours(conn: &Connection) -> rusqlite::Result<bool> {
    let app_id: i32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if app_id == APPLICATION_ID {
        return Ok(true);
    }
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if tables.is_empty() {
        return Ok(app_id == 0);
    }
    // Libraries made before the stamp: they have our bookkeeping tables.
    Ok(app_id == 0 && tables.iter().any(|t| t == "meta") && tables.iter().any(|t| t == "folders"))
}

/// Connection settings used by every connection.
pub fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;",
    )
}

/// Brings the database up to [`SCHEMA_VERSION`]. Each step runs in its own
/// transaction, so a failure leaves the previous version intact.
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as u32)?;
        tx.commit()?;
        tracing::info!("library database migrated to v{}", i + 1);
    }
    conn.pragma_update(None, "application_id", APPLICATION_ID)?;
    Ok(())
}
