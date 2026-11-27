//! Prints "Play similar" results for tracks whose title contains a word.
//!
//! `cargo run --release -p lumen-reco --example similar -- <library.db> <title> [title…]`

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use lumen_reco::{Reco, Snapshot};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: similar <library.db> <title> [title…]");
        std::process::exit(2);
    }
    let db = PathBuf::from(&args[0]);
    let catalog = lumen_library::Reader::open(&db)
        .and_then(|r| r.catalog())
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1)
        });
    let reco = Reco::open(&db, false).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let snap = Snapshot::from_catalog(&catalog);
    let now = lumen_library::db::now_ms();
    let name = |id| {
        catalog
            .index_of(id)
            .map(|i| {
                let t = catalog.track(i);
                match &t.genre {
                    Some(g) => format!("{} — {}  [{g}]", t.title, t.artist),
                    None => format!("{} — {}", t.title, t.artist),
                }
            })
            .unwrap_or_default()
    };
    for word in &args[1..] {
        let needle = word.to_lowercase();
        let Some(seed) = catalog
            .tracks
            .iter()
            .find(|t| t.track.title.to_lowercase().contains(&needle))
            .map(|t| t.track.id)
        else {
            println!("\nno title contains {word:?}");
            continue;
        };
        let started = Instant::now();
        let recs = reco.recommend(&snap, &[seed], 24, &HashSet::new(), now, true);
        let took = started.elapsed();
        let model = reco.model();
        let sv = model.index.get(seed).copied();
        println!(
            "\nPlay similar: {}  ({:.2} ms)",
            name(seed),
            took.as_secs_f64() * 1000.0
        );
        for (k, id) in recs.iter().take(10).enumerate() {
            let d = match (sv, model.index.get(*id)) {
                (Some(a), Some(b)) => format!("{:.2}", model.index.distance(&a, b)),
                _ => "  -".into(),
            };
            println!("  {:>2}. {}   (distance {d})", k + 1, name(*id));
        }
    }
}
