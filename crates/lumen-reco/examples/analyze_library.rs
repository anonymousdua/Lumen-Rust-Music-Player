//! Analyzes a library database's tracks with the real background pool and
//! reports throughput.
//!
//! `cargo run --release -p lumen-reco --example analyze_library -- <library.db>`
//!
//! Use a copy of the database: it adds analysis rows (migrating it first if
//! it is older).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lumen_reco::{Reco, RecoEvent};

fn main() {
    let Some(db) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: analyze_library <library.db>");
        std::process::exit(2);
    };
    // Migrates the schema if needed.
    if let Err(e) = lumen_library::db::Db::open(&db) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    let reco = match Reco::open(&db, true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let before = reco.status();
    println!(
        "{} of {} tracks already analyzed; {} worker threads",
        before.done,
        before.total,
        lumen_reco::analyze::worker_count()
    );
    let started = Instant::now();
    reco.refresh();
    let mut last_print = Instant::now();
    let mut seen_running = false;
    loop {
        match reco.events().recv_timeout(Duration::from_secs(600)) {
            Ok(RecoEvent::RunFinished {
                analyzed,
                failed,
                elapsed,
            }) => {
                let secs = elapsed.as_secs_f64();
                println!(
                    "
analyzed {analyzed} tracks in {secs:.1} s ({:.0} tracks/min), {failed} failed",
                    analyzed as f64 / secs * 60.0
                );
                break;
            }
            Ok(RecoEvent::Analysis) => {
                let s = reco.status();
                seen_running |= s.running;
                if !seen_running && !s.running && started.elapsed() > Duration::from_secs(2) {
                    println!("nothing to analyze");
                    break;
                }
                if last_print.elapsed() > Duration::from_secs(15) {
                    last_print = Instant::now();
                    println!(
                        "  {} / {}  ({:.0} s)",
                        s.done,
                        s.total,
                        started.elapsed().as_secs_f64()
                    );
                }
            }
            Ok(_) => {}
            Err(_) => {
                println!("timed out");
                break;
            }
        }
    }
    let s = reco.status();
    println!(
        "status: {} / {} analyzed, {} failed",
        s.done, s.total, s.failed
    );
}
