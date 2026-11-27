//! The recommendation tables on a real (migrated) library database.

use std::path::{Path, PathBuf};

use lumen_core::TrackId;
use lumen_reco::features::DIM;
use lumen_reco::signals::{Outcome, PlayRecord, Signals};
use lumen_reco::store::Store;
use rusqlite::{Connection, params};

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-reco-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let db = dir.join("library.db");
    // Creates and migrates the library schema.
    drop(lumen_library::db::Db::open(&db).expect("library db"));
    db
}

fn add_tracks(db: &Path, n: i64) {
    let c = Connection::open(db).expect("open");
    c.execute(
        "INSERT INTO folders (id, path, added_at) VALUES (1, 'D:\\m', 0)",
        [],
    )
    .expect("folder");
    for id in 1..=n {
        c.execute(
            "INSERT INTO tracks (id, path, folder_id, size, mtime, date_added, title, artist,
                                 album, duration_ms, codec, sort_title, sort_artist, sort_album)
             VALUES (?1, ?2, 1, 1000, 5000, 0, 't', 'a', 'b', 200000, 'FLAC', x'', x'', x'')",
            params![id, format!("D:\\m\\{id}.flac")],
        )
        .expect("track");
    }
}

fn play(id: i64, start: i64, listened_s: u64) -> PlayRecord {
    PlayRecord {
        track: TrackId(id),
        started_at: start,
        ended_at: start + listened_s as i64 * 1000,
        listened_ms: listened_s * 1000,
        duration_ms: 200_000,
        reached_end: listened_s >= 190,
        interrupted: false,
    }
}

#[test]
fn analysis_rows_follow_file_changes_and_versions() {
    let db = temp_db("analysis");
    add_tracks(&db, 3);
    let store = Store::open(&db).expect("store");
    let pending = store.pending().expect("pending");
    assert_eq!(pending.len(), 3);
    assert_eq!(store.analysis_counts().expect("counts"), (0, 3));

    let v = [0.25f32; DIM];
    store.save_vector(&pending[0], &v, 10).expect("save");
    store.save_vector(&pending[1], &v, 10).expect("save");
    assert_eq!(store.pending().expect("pending").len(), 1);
    assert_eq!(store.vectors().expect("vectors").len(), 2);
    assert_eq!(store.analysis_counts().expect("counts"), (2, 3));

    // The file changed: analyzed again.
    let c = Connection::open(&db).expect("open");
    c.execute("UPDATE tracks SET mtime = 6000 WHERE id = 1", [])
        .expect("touch");
    let ids: Vec<i64> = store
        .pending()
        .expect("pending")
        .iter()
        .map(|p| p.id.0)
        .collect();
    assert_eq!(ids, [1, 3]);

    // An older feature version doesn't count.
    c.execute(
        "UPDATE track_analysis SET features_version = 1 WHERE track_id = 2",
        [],
    )
    .expect("version");
    assert_eq!(store.pending().expect("pending").len(), 3);
    // Track 1's vector stays usable until its re-analysis replaces it.
    assert_eq!(store.vectors().expect("vectors").len(), 1);

    // "Re-analyze library" redoes everything analyzed before it.
    let fresh = store.pending().expect("pending");
    for p in &fresh {
        store.save_vector(p, &v, 20).expect("save");
    }
    assert!(store.pending().expect("pending").is_empty());
    store.request_reanalysis(30).expect("reanalyze");
    assert_eq!(store.pending().expect("pending").len(), 3);

    // Removing a track removes its analysis.
    c.execute("DELETE FROM tracks WHERE id = 3", [])
        .expect("delete");
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM track_analysis", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 2);
    // Saving a vector for a removed track is a no-op, not an error.
    store.save_vector(&fresh[2], &v, 40).expect("gone is fine");
}

#[test]
fn listening_history_round_trips_and_resets() {
    let db = temp_db("history");
    add_tracks(&db, 4);
    let mut store = Store::open(&db).expect("store");
    let mut signals = Signals::default();
    let plays = [
        play(1, 0, 200),
        play(2, 300_000, 120),
        play(3, 500_000, 5),
        play(4, 600_000, 200),
    ];
    for p in &plays {
        let applied = signals.record(p, 22);
        store.save_play(p, 22, &applied).expect("save play");
    }
    assert_eq!(store.play_event_count().expect("count"), 4);

    let loaded = store.signals().expect("signals");
    assert_eq!(loaded.stats, signals.stats);
    for (a, b) in [(1, 2), (1, 4), (2, 4), (1, 3)] {
        assert_eq!(
            loaded.coplay(TrackId(a), TrackId(b)),
            signals.coplay(TrackId(a), TrackId(b)),
            "{a}-{b}"
        );
    }
    assert_eq!(loaded.coplay(TrackId(1), TrackId(4)), 1);
    assert_eq!(loaded.stats[&TrackId(3)].skips, 1);

    // The session carries over a restart: the next play pairs with all three heard tracks.
    let mut resumed = loaded;
    let next = resumed.record(&play(2, 900_000, 200), 23);
    assert_eq!(next.session_id, 0);
    assert!(next.pairs.is_empty(), "2 was already heard this session");
    let other = resumed.record(&play(3, 1_200_000, 200), 23);
    assert_eq!(other.outcome, Outcome::Completed);
    assert_eq!(other.pairs, [(1, 3), (2, 3), (3, 4)]);

    store
        .save_mix("late_night", 77, &[TrackId(4), TrackId(2)])
        .expect("mix");
    assert_eq!(
        store.mix("late_night").expect("mix"),
        Some((77, vec![TrackId(4), TrackId(2)]))
    );

    store.reset_history().expect("reset");
    assert_eq!(store.play_event_count().expect("count"), 0);
    let empty = store.signals().expect("signals");
    assert!(empty.stats.is_empty());
    assert_eq!(empty.coplay(TrackId(1), TrackId(2)), 0);
    assert_eq!(store.mix("late_night").expect("mix"), None);
}

#[test]
fn plays_of_removed_tracks_are_dropped() {
    let db = temp_db("removed");
    add_tracks(&db, 2);
    let mut store = Store::open(&db).expect("store");
    let mut signals = Signals::default();
    let p = play(99, 0, 200);
    let applied = signals.record(&p, 10);
    store
        .save_play(&p, 10, &applied)
        .expect("missing track is not an error");
    assert_eq!(store.play_event_count().expect("count"), 0);
}
