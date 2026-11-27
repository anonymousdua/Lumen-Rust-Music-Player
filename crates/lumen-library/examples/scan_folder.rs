//! Scans a music folder into a library data directory and prints timings:
//!
//! ```text
//! cargo run --release -p lumen-library --example scan_folder -- <DATA_DIR> <MUSIC_DIR>
//! ```
//!
//! Then run the app with `LUMEN_DATA_DIR=<DATA_DIR>` to browse that library.

use std::path::Path;
use std::time::{Duration, Instant};

use lumen_library::{Library, LibraryConfig, LibraryEvent, LibraryPaths};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(data), Some(music)) = (args.next(), args.next()) else {
        eprintln!("usage: scan_folder <DATA_DIR> <MUSIC_DIR>");
        std::process::exit(2);
    };
    let config = LibraryConfig {
        paths: LibraryPaths::in_dir(Path::new(&data)),
        watch: false,
        scan_on_open: false,
        ..LibraryConfig::default()
    };
    let lib = Library::open(config).expect("open library");
    let started = Instant::now();
    lib.add_folder(&music);
    loop {
        match lib.events().recv_timeout(Duration::from_secs(600)) {
            Ok(LibraryEvent::ScanFinished {
                added,
                updated,
                removed,
                ..
            }) => {
                println!(
                    "scan: {added} added, {updated} updated, {removed} removed in {:.2} s",
                    started.elapsed().as_secs_f64()
                );
                break;
            }
            Ok(LibraryEvent::Error(e)) => eprintln!("error: {e}"),
            Ok(_) => {}
            Err(e) => {
                eprintln!("no scan result: {e}");
                break;
            }
        }
    }
    let t = Instant::now();
    let reader = lib.reader().expect("reader");
    let catalog = reader.catalog().expect("catalog");
    println!(
        "catalog: {} tracks, {} albums, {} artists read in {:.1} ms",
        catalog.tracks.len(),
        catalog.albums.len(),
        catalog.artists.len(),
        t.elapsed().as_secs_f64() * 1000.0
    );
    let t = Instant::now();
    let rows = catalog.songs(
        lumen_core::SongSort::Title,
        lumen_core::SortDir::Asc,
        lumen_core::SongFilter::All,
        "",
    );
    println!(
        "sort {} rows by title: {:.1} ms",
        rows.len(),
        t.elapsed().as_secs_f64() * 1000.0
    );
    let t = Instant::now();
    let hits = catalog.songs(
        lumen_core::SongSort::DateModified,
        lumen_core::SortDir::Desc,
        lumen_core::SongFilter::All,
        "nandu",
    );
    println!(
        "search \"nandu\": {} hits in {:.1} ms",
        hits.len(),
        t.elapsed().as_secs_f64() * 1000.0
    );
}
