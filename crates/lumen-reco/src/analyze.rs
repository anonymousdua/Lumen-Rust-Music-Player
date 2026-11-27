//! The analysis pool: a few background-priority threads that decode and
//! analyze tracks with bliss-rs, one file each at a time.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use bliss_audio::decoder::Decoder as _;
use bliss_audio::decoder::symphonia::SymphoniaDecoder;
use crossbeam_channel::{Receiver, Sender};

use crate::features::{self, Vector};
use crate::store::Pending;

/// Most worker threads, whatever the core count. bliss decodes a whole
/// song into memory (about 150 MB for four minutes of CD audio, 330 MB for
/// the longest hi-res files measured), so more threads cost a lot of memory
/// for little gain on a background job.
pub const MAX_WORKERS: usize = 4;

/// `cores − 1`, at least 1 and at most [`MAX_WORKERS`].
pub fn worker_count() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    cores.saturating_sub(1).clamp(1, MAX_WORKERS)
}

/// Analyzes one file. Never panics (bliss panics on some odd channel
/// layouts; that is caught and reported as a failure).
pub fn analyze_file(path: &Path) -> Result<Vector, String> {
    let result = std::panic::catch_unwind(|| SymphoniaDecoder::song_from_path(path));
    match result {
        Ok(Ok(song)) => features::from_analysis(&song.analysis)
            .ok_or_else(|| "analysis produced invalid values".to_string()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(panic) => Err(match panic.downcast_ref::<&str>() {
            Some(s) => format!("analysis crashed: {s}"),
            None => match panic.downcast_ref::<String>() {
                Some(s) => format!("analysis crashed: {s}"),
                None => "analysis crashed".to_string(),
            },
        }),
    }
}

/// One file's result.
pub struct Done {
    pub job: Pending,
    pub result: Result<Vector, String>,
}

/// Workers taking jobs from a shared queue. Pausing holds them before
/// their next file; stopping makes them quit after the current one.
pub struct Pool {
    threads: Vec<JoinHandle<()>>,
    pub stop: Arc<AtomicBool>,
}

impl Pool {
    pub fn start(
        jobs: Vec<Pending>,
        workers: usize,
        paused: Arc<AtomicBool>,
        results: Sender<Done>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::unbounded();
        for j in jobs {
            let _ = tx.send(j);
        }
        drop(tx);
        let mut threads = Vec::new();
        for n in 0..workers {
            let (rx, results, paused, stop) =
                (rx.clone(), results.clone(), paused.clone(), stop.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("lumen-analysis-{n}"))
                .spawn(move || work(rx, results, paused, stop));
            match spawned {
                Ok(t) => threads.push(t),
                Err(e) => tracing::warn!("cannot start an analysis thread: {e}"),
            }
        }
        Self { threads, stop }
    }

    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    /// Lets the workers finish their current file and exit, without waiting.
    pub fn cancel(self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn join(self) {
        for t in self.threads {
            let _ = t.join();
        }
    }
}

fn work(
    jobs: Receiver<Pending>,
    results: Sender<Done>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    crate::sys::background_thread();
    loop {
        while paused.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(250));
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(job) = jobs.recv() else { return };
        let result = analyze_file(&job.path);
        if results.send(Done { job, result }).is_err() {
            return;
        }
    }
}
