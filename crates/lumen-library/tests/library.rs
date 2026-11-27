//! End-to-end library tests on real files: FLACs with Unicode tags and
//! embedded covers, `.lrc` files, rescans and the folder watcher.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use flacenc::component::BitRepr;
use flacenc::error::Verify;
use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::Tag;
use lumen_core::{LyricsFlags, SortDir};
use lumen_library::{Library, LibraryConfig, LibraryEvent, LibraryPaths, Reader, TrackSort};

fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lumen-lib-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A short stereo 16/44.1 FLAC (a few hundred bytes of audio).
fn flac_bytes(frames: usize, seed: i32) -> Vec<u8> {
    let samples: Vec<i32> = (0..frames * 2)
        .map(|i| (((i as i32).wrapping_mul(7919) ^ seed) % 2000) - 1000)
        .collect();
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .expect("config");
    let source = flacenc::source::MemSource::from_samples(&samples, 2, 16, 44_100);
    let stream =
        flacenc::encode_with_fixed_block_size(&config, source, config.block_size).expect("encode");
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).expect("write");
    let mut b = sink.as_slice().to_vec();
    // libFLAC convention: min block size == max block size (see lumen-audio tests).
    let max = [b[10], b[11]];
    b[8..10].copy_from_slice(&max);
    b
}

struct Tags<'a> {
    title: &'a str,
    artist: &'a str,
    album: &'a str,
    album_artist: Option<&'a str>,
    track: u32,
    year: Option<&'a str>,
    cover: Option<&'a [u8]>,
    lyrics: Option<&'a str>,
}

fn write_flac(path: &Path, t: &Tags<'_>, seed: i32) {
    std::fs::write(path, flac_bytes(4_096 + seed as usize % 3_000, seed)).expect("write flac");
    retag(path, t);
}

fn retag(path: &Path, t: &Tags<'_>) {
    let mut file = lofty::read_from_path(path).expect("read back");
    if file.primary_tag().is_none() {
        file.insert_tag(Tag::new(file.primary_tag_type()));
    }
    let tag = file.primary_tag_mut().expect("tag");
    tag.set_title(t.title.to_string());
    tag.set_artist(t.artist.to_string());
    tag.set_album(t.album.to_string());
    tag.set_track(t.track);
    if let Some(a) = t.album_artist {
        tag.insert_text(ItemKey::AlbumArtist, a.to_string());
    }
    if let Some(y) = t.year {
        tag.insert_text(ItemKey::Year, y.to_string());
    }
    if let Some(l) = t.lyrics {
        tag.insert_text(ItemKey::Lyrics, l.to_string());
    }
    if let Some(c) = t.cover {
        tag.push_picture(
            Picture::unchecked(c.to_vec())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Jpeg)
                .build(),
        );
    }
    file.save_to_path(path, WriteOptions::default())
        .expect("save tags");
}

fn jpeg(size: u32, shade: u8) -> Vec<u8> {
    let img = image::RgbImage::from_fn(size, size, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, shade])
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&img)
        .expect("jpeg");
    out
}

fn open(data: &Path, watch: bool) -> Library {
    Library::open(LibraryConfig {
        paths: LibraryPaths::in_dir(data),
        watch,
        scan_on_open: false,
        workers: 4,
        debounce: Duration::from_millis(200),
        track_mtime: true,
    })
    .expect("open library")
}

fn wait_event(
    lib: &Library,
    timeout: Duration,
    mut pred: impl FnMut(&LibraryEvent) -> bool,
) -> Vec<LibraryEvent> {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match lib.events().recv_timeout(left) {
            Ok(e) => {
                let done = pred(&e);
                seen.push(e);
                if done {
                    return seen;
                }
            }
            Err(_) => break,
        }
    }
    panic!("timed out; events: {seen:#?}");
}

fn finished(lib: &Library) -> (usize, usize, usize, Vec<LibraryEvent>) {
    let events = wait_event(lib, Duration::from_secs(30), |e| {
        matches!(e, LibraryEvent::ScanFinished { .. })
    });
    match events.last() {
        Some(LibraryEvent::ScanFinished {
            added,
            updated,
            removed,
            ..
        }) => (*added, *updated, *removed, events),
        _ => unreachable!(),
    }
}

/// Polls `cond` against a fresh reader until it holds (watcher tests).
fn eventually(lib: &Library, what: &str, cond: impl Fn(&Reader) -> bool) {
    let reader = lib.reader().expect("reader");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if cond(&reader) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("never happened: {what}");
}

/// Two albums in Hindi and Malayalam plus Spanish/English singles.
fn make_music(root: &Path) -> PathBuf {
    let music = root.join("Music");
    let a = music.join("Arijit").join("रात की धुन");
    let b = music.join("ചിത്ര").join("നിലാവ്");
    let c = music.join("Singles");
    for d in [&a, &b, &c] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    let cover_a = jpeg(600, 40);
    let titles_a = ["तुम ही हो", "अजनबी", "चाँद", "सपने"];
    for (i, t) in titles_a.iter().enumerate() {
        write_flac(
            &a.join(format!("{:02}.flac", i + 1)),
            &Tags {
                title: t,
                artist: "अरिजीत",
                album: "रात की धुन",
                album_artist: Some("अरिजीत"),
                track: i as u32 + 1,
                year: Some("2023-04-01"),
                cover: Some(&cover_a),
                lyrics: None,
            },
            i as i32 + 1,
        );
    }
    for (i, t) in ["അമ്മ", "നിലാവ്", "മഴ"].iter().enumerate() {
        write_flac(
            &b.join(format!("{:02}.flac", i + 1)),
            &Tags {
                title: t,
                artist: "ചിത്ര",
                album: "നിലാവ്",
                album_artist: None,
                track: i as u32 + 1,
                year: Some("2019"),
                cover: None,
                lyrics: if i == 2 {
                    Some("[00:01.00]മഴ പെയ്യുന്നു")
                } else {
                    None
                },
            },
            i as i32 + 10,
        );
    }
    // Folder art for album B.
    std::fs::write(b.join("cover.jpg"), jpeg(400, 200)).expect("folder art");
    for (i, (t, ar)) in [
        ("Ñandú", "Los Ñ"),
        ("Árbol", "Ana"),
        ("apple", "Zed"),
        ("Track 10", "Zed"),
        ("Track 2", "Zed"),
    ]
    .iter()
    .enumerate()
    {
        write_flac(
            &c.join(format!("{t}.flac")),
            &Tags {
                title: t,
                artist: ar,
                album: "Singles",
                album_artist: Some("Various"),
                track: i as u32 + 1,
                year: None,
                cover: None,
                lyrics: None,
            },
            i as i32 + 20,
        );
    }
    std::fs::write(
        a.join("01.lrc"),
        "[ar:x]\n[00:01.00]<00:01.00>तुम <00:01.50>ही\n",
    )
    .expect("lrc");
    std::fs::write(a.join("02.lrc"), "plain lyrics, no timestamps\n").expect("lrc");
    std::fs::write(c.join("junk.flac"), b"not a flac file at all").expect("junk");
    std::fs::write(c.join("notes.txt"), b"ignored").expect("txt");
    music
}

#[test]
fn first_scan_streams_and_reads_everything() {
    let root = temp_dir("scan");
    let music = make_music(&root);
    let lib = open(&root.join("data"), false);
    lib.add_folder(&music);
    let (added, updated, removed, events) = finished(&lib);
    assert_eq!(
        (added, updated, removed),
        (12, 0, 0),
        "junk.flac is skipped"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LibraryEvent::ScanProgress { done: 0, total: 13 }))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        LibraryEvent::ScanProgress {
            done: 13,
            total: 13
        }
    )));

    let r = lib.reader().expect("reader");
    let stats = r.stats().expect("stats");
    assert_eq!(stats.tracks, 12);
    assert_eq!(stats.albums, 3);
    assert_eq!(stats.artists, 5);
    assert_eq!(stats.with_synced_lyrics, 2, "enhanced .lrc + embedded LRC");
    assert!(stats.last_scan_ms.is_some());

    let folders = r.folders().expect("folders");
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].tracks, 12);
    assert!(folders[0].available);

    // Unicode collation: Latin (ñ after n, numbers numeric), Devanagari, Malayalam.
    let titles: Vec<String> = r
        .tracks(TrackSort::Title, SortDir::Asc)
        .expect("tracks")
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(
        titles,
        vec![
            "apple",
            "Árbol",
            "Ñandú",
            "Track 2",
            "Track 10",
            "अजनबी",
            "चाँद",
            "तुम ही हो",
            "सपने",
            "അമ്മ",
            "നിലാവ്",
            "മഴ"
        ]
    );
    assert_eq!(
        r.album_titles().expect("albums"),
        vec!["Singles", "रात की धुन", "നിലാവ്"]
    );

    // The in-memory catalog sorts the same way the database does.
    let cat = r.catalog().expect("catalog");
    let cat_titles: Vec<String> = cat
        .songs(
            lumen_core::SongSort::Title,
            SortDir::Asc,
            lumen_core::SongFilter::All,
            "",
        )
        .into_iter()
        .map(|i| cat.track(i).title.clone())
        .collect();
    assert_eq!(cat_titles, titles);
    for sort in lumen_core::SongSort::ALL {
        for dir in [SortDir::Asc, SortDir::Desc] {
            let db: Vec<_> = r
                .tracks(sort, dir)
                .expect("tracks")
                .into_iter()
                .map(|t| t.id)
                .collect();
            let mem = cat.ids(&cat.songs(sort, dir, lumen_core::SongFilter::All, ""));
            assert_eq!(db, mem, "{sort:?} {dir:?}");
        }
    }
    assert_eq!(cat.albums.len(), 3);
    assert_eq!(cat.artists.len() as u64, stats.artists);
    let album_names: Vec<String> = cat
        .album_list(lumen_core::AlbumSort::Title, SortDir::Asc, "")
        .into_iter()
        .map(|i| cat.albums[i].title.clone())
        .collect();
    assert_eq!(album_names, vec!["Singles", "रात की धुन", "നിലാവ്"]);

    // Tags, properties, lyrics flags.
    let t = r
        .track_by_path(&music.join("Arijit").join("रात की धुन").join("01.flac"))
        .expect("q")
        .expect("track");
    assert_eq!(t.title, "तुम ही हो");
    assert_eq!(t.artist, "अरिजीत");
    assert_eq!(t.year, Some(2023));
    assert_eq!(t.track_no, Some(1));
    assert_eq!(
        (t.codec.as_str(), t.sample_rate, t.bit_depth),
        ("FLAC", Some(44_100), Some(16))
    );
    assert!(
        t.lyrics.has(LyricsFlags::LRC)
            && t.lyrics.has(LyricsFlags::LRC_ENHANCED)
            && t.lyrics.synced()
    );
    let t2 = r
        .track_by_path(&music.join("Arijit").join("रात की धुन").join("02.flac"))
        .expect("q")
        .expect("track");
    assert!(t2.lyrics.has(LyricsFlags::LRC) && !t2.lyrics.synced());
    let t3 = r
        .track_by_path(&music.join("ചിത്ര").join("നിലാവ്").join("03.flac"))
        .expect("q")
        .expect("track");
    assert!(t3.lyrics.has(LyricsFlags::EMBEDDED_SYNCED));
    // The player's lyrics loader reads the same tag as synced lyrics.
    let embedded = lumen_lyrics::read_embedded(&t3.path).expect("embedded lyrics");
    assert!(embedded.synced && !embedded.is_empty());

    // Covers: embedded for album A, folder art for album B, one thumbnail each.
    let cover_a = r
        .album_cover("रात की धुन")
        .expect("q")
        .expect("embedded cover");
    let cover_b = r.album_cover("നിലാവ്").expect("q").expect("folder art");
    assert_ne!(cover_a, cover_b);
    for h in [&cover_a, &cover_b] {
        let thumb = image::open(lib.thumb_path(h)).expect("thumbnail exists");
        assert!(thumb.width() <= 320 && thumb.height() <= 320);
    }
    assert_eq!(r.album_cover("Singles").expect("q"), None);
}

#[test]
fn rescans_are_incremental_and_keep_history() {
    let root = temp_dir("rescan");
    let music = make_music(&root);
    let lib = open(&root.join("data"), false);
    lib.add_folder(&music);
    finished(&lib);
    let r = lib.reader().expect("reader");

    // Nothing changed: nothing is re-read.
    lib.rescan();
    let (a, u, d, events) = finished(&lib);
    assert_eq!((a, u, d), (0, 0, 0));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LibraryEvent::ScanProgress { total: 1, .. })),
        "only junk.flac retried"
    );

    let singles = music.join("Singles");
    let apple = r
        .track_by_path(&singles.join("apple.flac"))
        .expect("q")
        .expect("apple");
    lib.record_play(apple.id);
    lib.set_lyrics_offset(apple.id, -500);

    // Edit tags: re-read.
    std::thread::sleep(Duration::from_millis(20));
    retag(
        &singles.join("Árbol.flac"),
        &Tags {
            title: "Árbol (Remaster)",
            artist: "Ana",
            album: "Singles",
            album_artist: Some("Various"),
            track: 2,
            year: Some("2024"),
            cover: None,
            lyrics: None,
        },
    );
    // Rename (move) a file: same id, play count and offset kept.
    std::fs::create_dir_all(singles.join("moved")).expect("dir");
    std::fs::rename(
        singles.join("apple.flac"),
        singles.join("moved").join("manzana.flac"),
    )
    .expect("rename");
    // Delete a file.
    std::fs::remove_file(singles.join("Track 2.flac")).expect("delete");
    // Add a synced .lrc to an unchanged file.
    std::fs::write(singles.join("Ñandú.lrc"), "[00:02.00]hola\n").expect("lrc");

    lib.rescan();
    let (a, u, d, _) = finished(&lib);
    assert_eq!((a, u, d), (0, 2, 1), "one edit + one move, one removal");

    let moved = r
        .track_by_path(&singles.join("moved").join("manzana.flac"))
        .expect("q")
        .expect("moved track");
    assert_eq!(moved.id, apple.id);
    assert_eq!(moved.play_count, 1);
    assert_eq!(moved.lyrics_offset_ms, -500);
    assert_eq!(moved.date_added_ms, apple.date_added_ms);
    let edited = r
        .track_by_path(&singles.join("Árbol.flac"))
        .expect("q")
        .expect("edited");
    assert_eq!(
        (edited.title.as_str(), edited.year),
        ("Árbol (Remaster)", Some(2024))
    );
    assert!(
        r.track_by_path(&singles.join("Track 2.flac"))
            .expect("q")
            .is_none()
    );
    let nandu = r
        .track_by_path(&singles.join("Ñandú.flac"))
        .expect("q")
        .expect("ñandú");
    assert!(nandu.lyrics.has(LyricsFlags::LRC_SYNCED));
    assert_eq!(r.stats().expect("stats").tracks, 11);
}

#[test]
fn watcher_picks_up_changes_live() {
    let root = temp_dir("watch");
    let music = make_music(&root);
    let lib = open(&root.join("data"), true);
    lib.add_folder(&music);
    finished(&lib);
    let singles = music.join("Singles");

    // A new file appears.
    let new = singles.join("Nueva.flac");
    write_flac(
        &new,
        &Tags {
            title: "Nueva canción",
            artist: "Ana",
            album: "Singles",
            album_artist: Some("Various"),
            track: 9,
            year: None,
            cover: None,
            lyrics: None,
        },
        77,
    );
    eventually(&lib, "new file indexed", |r| {
        r.track_by_path(&new)
            .ok()
            .flatten()
            .is_some_and(|t| t.title == "Nueva canción")
    });
    let id = lib
        .reader()
        .expect("r")
        .track_by_path(&new)
        .expect("q")
        .expect("t")
        .id;

    // Its tags are edited.
    retag(
        &new,
        &Tags {
            title: "Nueva canción (Live)",
            artist: "Ana",
            album: "Singles",
            album_artist: Some("Various"),
            track: 9,
            year: None,
            cover: None,
            lyrics: None,
        },
    );
    eventually(&lib, "edit picked up", |r| {
        r.track_by_path(&new)
            .ok()
            .flatten()
            .is_some_and(|t| t.title == "Nueva canción (Live)")
    });

    // An .lrc appears next to it.
    std::fs::write(singles.join("Nueva.lrc"), "[00:01.00]hola\n").expect("lrc");
    eventually(&lib, ".lrc noticed", |r| {
        r.track_by_path(&new)
            .ok()
            .flatten()
            .is_some_and(|t| t.lyrics.synced())
    });

    // It is renamed: same id.
    let renamed = singles.join("Renamed.flac");
    std::fs::rename(&new, &renamed).expect("rename");
    eventually(&lib, "rename tracked", |r| {
        r.track_by_path(&renamed)
            .ok()
            .flatten()
            .is_some_and(|t| t.id == id)
            && r.track_by_path(&new).ok().flatten().is_none()
    });

    // A whole folder is renamed.
    let album_dir = music.join("Arijit").join("रात की धुन");
    let album_new = music.join("Arijit").join("Raat");
    std::fs::rename(&album_dir, &album_new).expect("rename dir");
    eventually(&lib, "folder rename tracked", |r| {
        r.track_by_path(&album_new.join("01.flac"))
            .ok()
            .flatten()
            .is_some()
            && r.track_by_path(&album_dir.join("01.flac"))
                .ok()
                .flatten()
                .is_none()
    });

    // It is deleted.
    std::fs::remove_file(&renamed).expect("delete");
    eventually(&lib, "delete noticed", |r| {
        r.track_by_path(&renamed).ok().flatten().is_none()
    });

    // A new album folder is copied in.
    let fresh = music.join("Fresh");
    std::fs::create_dir_all(&fresh).expect("dir");
    for i in 0..3 {
        write_flac(
            &fresh.join(format!("{i}.flac")),
            &Tags {
                title: "x",
                artist: "y",
                album: "Fresh",
                album_artist: None,
                track: i + 1,
                year: None,
                cover: None,
                lyrics: None,
            },
            90 + i as i32,
        );
    }
    eventually(&lib, "new folder indexed", |r| {
        (0..3).all(|i| {
            r.track_by_path(&fresh.join(format!("{i}.flac")))
                .ok()
                .flatten()
                .is_some()
        })
    });
}

#[test]
fn offline_folder_keeps_its_tracks() {
    let root = temp_dir("offline");
    let music = make_music(&root);
    let lib = open(&root.join("data"), false);
    lib.add_folder(&music);
    finished(&lib);
    let away = root.join("Unplugged");
    std::fs::rename(&music, &away).expect("unplug");
    lib.rescan();
    let (_, _, removed, _) = finished(&lib);
    assert_eq!(removed, 0, "an unavailable folder must not be pruned");
    let r = lib.reader().expect("reader");
    assert_eq!(r.stats().expect("stats").tracks, 12);
    assert!(!r.folders().expect("folders")[0].available);
}

#[test]
fn nested_folders_and_reopen() {
    let root = temp_dir("nested");
    let music = make_music(&root);
    let data = root.join("data");
    {
        let lib = open(&data, false);
        lib.add_folder(music.join("Singles"));
        finished(&lib);
        // A folder inside an existing one is rejected.
        lib.add_folder(music.join("Singles").join("..").join("Singles"));
        wait_event(&lib, Duration::from_secs(10), |e| {
            matches!(e, LibraryEvent::Error(_))
        });
        // A parent absorbs the child folder.
        lib.add_folder(&music);
        finished(&lib);
        let r = lib.reader().expect("reader");
        let folders = r.folders().expect("folders");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].tracks, 12);
    }
    // Reopening keeps everything (migrations are idempotent).
    let lib = open(&data, false);
    let r = lib.reader().expect("reader");
    assert_eq!(r.stats().expect("stats").tracks, 12);
    // Removing the folder removes its tracks, albums and artists.
    let id = r.folders().expect("folders")[0].id;
    lib.remove_folder(id);
    wait_event(&lib, Duration::from_secs(10), |e| {
        matches!(e, LibraryEvent::FoldersChanged)
    });
    let s = r.stats().expect("stats");
    assert_eq!((s.tracks, s.albums, s.artists, s.folders), (0, 0, 0, 0));
}

/// `cargo test -p lumen-library --release --test library -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_2500_tracks() {
    let root = temp_dir("perf");
    let music = root.join("Music");
    let base = flac_bytes(4_096, 3);
    for album in 0..209 {
        let dir = music
            .join(format!("Artist {}", album % 90))
            .join(format!("Album {album}"));
        std::fs::create_dir_all(&dir).expect("dir");
        let cover = jpeg(500, (album % 255) as u8);
        for t in 0..12 {
            let p = dir.join(format!("{t:02} - Song.flac"));
            std::fs::write(&p, &base).expect("write");
            retag(
                &p,
                &Tags {
                    title: &format!("गीत {album} {t}"),
                    artist: &format!("Artist {}", album % 90),
                    album: &format!("Album {album}"),
                    album_artist: None,
                    track: t + 1,
                    year: Some("2020"),
                    cover: Some(&cover),
                    lyrics: None,
                },
            );
        }
    }
    let data = root.join("data");
    let lib = open(&data, false);
    let t = Instant::now();
    lib.add_folder(&music);
    finished(&lib);
    println!("first scan of 2508 files: {:?}", t.elapsed());
    let t = Instant::now();
    lib.rescan();
    finished(&lib);
    println!("no-change rescan: {:?}", t.elapsed());
    drop(lib);
    let t = Instant::now();
    let r = lumen_library::Reader::open(&LibraryPaths::in_dir(&data).db).expect("reader");
    let s = r.stats().expect("stats");
    println!(
        "cold open + stats: {:?} ({} tracks, {} albums)",
        t.elapsed(),
        s.tracks,
        s.albums
    );
    let t = Instant::now();
    let all = r.tracks(TrackSort::Title, SortDir::Asc).expect("tracks");
    println!("load all {} tracks sorted: {:?}", all.len(), t.elapsed());
}

#[test]
fn playlists_reorder_remove_import_rename_delete() {
    let root = temp_dir("playlists");
    let music = make_music(&root);
    let lib = open(&root.join("data"), false);
    lib.add_folder(&music);
    finished(&lib);
    let r = lib.reader().expect("reader");
    let singles = music.join("Singles");
    let id_of = |name: &str| {
        r.track_by_path(&singles.join(name))
            .expect("q")
            .expect(name)
            .id
    };
    let (a, b, c) = (
        id_of("apple.flac"),
        id_of("Árbol.flac"),
        id_of("Ñandú.flac"),
    );

    lib.new_playlist("Mix", vec![a, b, c]);
    let events = wait_event(&lib, Duration::from_secs(10), |e| {
        matches!(e, LibraryEvent::PlaylistCreated { .. })
    });
    let Some(LibraryEvent::PlaylistCreated { id, .. }) = events.last().cloned() else {
        unreachable!()
    };
    let order = || {
        r.playlist_entries(id)
            .expect("entries")
            .into_iter()
            .map(|e| e.track)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(), vec![a, b, c]);

    // Drag the first entry to the end, then remove the middle one.
    lib.move_in_playlist(id, 0, 2);
    eventually(&lib, "reordered", |_| order() == vec![b, c, a]);
    lib.remove_from_playlist(id, 1);
    eventually(&lib, "removed", |_| order() == vec![b, a]);

    lib.rename_playlist(id, "मेरी प्लेलिस्ट");
    eventually(&lib, "renamed", |r| {
        r.playlists()
            .expect("pl")
            .iter()
            .any(|p| p.name == "मेरी प्लेलिस्ट" && p.tracks == 2)
    });

    // Import an .m3u8 with a relative path, an absolute one and a missing file.
    let m3u = root.join("lists").join("Road trip.m3u8");
    std::fs::create_dir_all(m3u.parent().expect("dir")).expect("dir");
    let rel = format!(
        r"..\{}\Singles\Ñandú.flac",
        music.file_name().expect("n").to_string_lossy()
    );
    std::fs::write(
        &m3u,
        format!(
            "#EXTM3U\n#EXTINF:1,x - y\n{rel}\n{}\nmissing/gone.flac\n",
            singles.join("apple.flac").display()
        ),
    )
    .expect("write m3u");
    lib.import_playlist(&m3u);
    let events = wait_event(&lib, Duration::from_secs(10), |e| {
        matches!(e, LibraryEvent::PlaylistCreated { .. })
    });
    let Some(LibraryEvent::PlaylistCreated {
        id: imported,
        added,
        missing,
    }) = events.last().cloned()
    else {
        unreachable!()
    };
    assert_eq!((added, missing), (2, 1));
    let tracks: Vec<_> = r
        .playlist_entries(imported)
        .expect("e")
        .into_iter()
        .map(|e| e.track)
        .collect();
    assert_eq!(tracks, vec![c, a]);
    assert!(
        r.playlists()
            .expect("pl")
            .iter()
            .any(|p| p.name == "Road trip")
    );

    lib.delete_playlist(id);
    eventually(&lib, "deleted", |r| {
        r.playlists().expect("pl").iter().all(|p| p.id != id)
    });
}

#[test]
fn date_modified_can_stay_put_when_files_change() {
    let root = temp_dir("date-modified");
    let music = make_music(&root);
    let lib = open(&root.join("data"), false);
    lib.add_folder(&music);
    finished(&lib);
    let r = lib.reader().expect("reader");
    let file = music.join("Singles").join("apple.flac");
    let first = r.track_by_path(&file).expect("q").expect("t").mtime_ms;
    let tags = Tags {
        title: "apple (edit)",
        artist: "Ana",
        album: "Singles",
        album_artist: None,
        track: 1,
        year: None,
        cover: None,
        lyrics: None,
    };

    // Off: the edit is read, but "Date modified" keeps its first value.
    lib.set_track_mtime(false);
    std::thread::sleep(Duration::from_millis(1100));
    retag(&file, &tags);
    lib.rescan();
    finished(&lib);
    let t = r.track_by_path(&file).expect("q").expect("t");
    assert_eq!(t.title, "apple (edit)");
    assert_eq!(t.mtime_ms, first);

    // On: it follows the file again.
    lib.set_track_mtime(true);
    std::thread::sleep(Duration::from_millis(1100));
    retag(
        &file,
        &Tags {
            title: "apple (2)",
            ..tags
        },
    );
    lib.rescan();
    finished(&lib);
    let t = r.track_by_path(&file).expect("q").expect("t");
    assert_eq!(t.title, "apple (2)");
    assert!(t.mtime_ms > first);
}

#[test]
fn a_database_from_another_program_is_set_aside_not_used() {
    // Like a prototype's library.db: user_version 2, unrelated tables.
    let root = temp_dir("foreign-db");
    let data = root.join("data");
    std::fs::create_dir_all(&data).expect("dir");
    let db = LibraryPaths::in_dir(&data).db;
    {
        let c = rusqlite::Connection::open(&db).expect("open");
        c.execute_batch(
            "CREATE TABLE tracks (id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE covers (id INTEGER PRIMARY KEY);
             INSERT INTO tracks(path) VALUES ('x');
             PRAGMA user_version = 2;",
        )
        .expect("old schema");
    }

    let music = make_music(&root);
    let lib = open(&data, false);
    let aside = lib.set_aside().expect("moved aside").to_path_buf();
    assert!(aside.exists(), "old database kept at {}", aside.display());
    let old = rusqlite::Connection::open(&aside).expect("old");
    let n: i64 = old
        .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
        .expect("old data intact");
    assert_eq!(n, 1);

    // The new library works.
    lib.add_folder(&music);
    finished(&lib);
    assert_eq!(lib.reader().expect("r").stats().expect("s").tracks, 12);
    drop(lib);

    // Reopening our own library does not move it.
    let lib = open(&data, false);
    assert!(lib.set_aside().is_none());
    assert_eq!(lib.reader().expect("r").stats().expect("s").tracks, 12);
}
