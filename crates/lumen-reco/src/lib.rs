//! Lumen's on-device recommendations: audio similarity from bliss-rs plus
//! personal listening signals. Nothing leaves the PC.
//!
//! - [`Reco::open`] loads the audio index and listening history from the
//!   library database and starts the recommendation thread, which records
//!   plays and runs the background analysis pool.
//! - [`Reco::recommend`] ranks tracks for seeds (Play similar, Radio, More
//!   like this); [`Reco::mixes`] returns the "Made for you" mixes.
//! - The scoring formula and its weights are in [`score`].
//!
//! Depends on `lumen-core` and `lumen-library`, never on GPUI.

pub mod analyze;
pub mod features;
pub mod index;
pub mod mixes;
pub mod model;
pub mod score;
pub mod signals;
pub mod store;
mod sys;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use lumen_core::TrackId;

pub use mixes::MixKind;
pub use model::{Model, Query, Snapshot, TrackMeta};
pub use score::Weights;
pub use signals::PlayRecord;

use crate::analyze::{Done, Pool};
use crate::features::Metric;
use crate::index::AudioIndex;
use crate::store::Store;

/// A mix is rebuilt at most this often (and on launch when older).
pub const MIX_EVERY_MS: i64 = 6 * 3_600_000;

/// Sound analysis, for the sidebar and Settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnalysisStatus {
    pub running: bool,
    /// Tracks with a current analysis.
    pub done: usize,
    pub total: usize,
    /// Files that could not be analyzed this session (retried on rescan).
    pub failed: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecoEvent {
    /// Analysis progress or state changed (see [`Reco::status`]).
    Analysis,
    /// New vectors or listening history: recommendations may differ.
    ModelChanged,
    /// An analysis run finished.
    RunFinished {
        analyzed: usize,
        failed: usize,
        elapsed: Duration,
    },
}

/// A "Made for you" mix.
#[derive(Clone, Debug, PartialEq)]
pub struct Mix {
    pub kind: MixKind,
    pub generated_at: i64,
    pub tracks: Vec<TrackId>,
}

enum Cmd {
    /// Pick up new and changed tracks.
    Refresh,
    /// Also retry files that failed.
    Rescan,
    Reanalyze,
    SetEnabled(bool),
    Record(PlayRecord),
    Shutdown,
}

/// Handle to the recommendation system. Cheap calls; recording and analysis
/// happen on its thread.
pub struct Reco {
    commands: Sender<Cmd>,
    events: Receiver<RecoEvent>,
    model: Arc<RwLock<Model>>,
    status: Arc<Mutex<AnalysisStatus>>,
    enabled: Arc<AtomicBool>,
    /// For the UI thread's small queries (mixes).
    store: Mutex<Store>,
    thread: Option<JoinHandle<()>>,
}

impl Reco {
    /// Loads vectors and history from `db` and starts the thread. Analysis
    /// waits for [`Reco::refresh`] (after the library scan).
    pub fn open(db: &Path, enabled: bool) -> Result<Self, String> {
        let started = Instant::now();
        let store = Store::open(db)?;
        let mut index = AudioIndex::new(Metric::bliss()?);
        for (id, v) in store.vectors()? {
            index.insert(id, v);
        }
        let signals = store.signals()?;
        let (done, total) = store.analysis_counts()?;
        tracing::info!(
            "recommendations: {} vectors, {} plays loaded in {:?}",
            index.len(),
            store.play_event_count().unwrap_or(0),
            started.elapsed()
        );
        let model = Arc::new(RwLock::new(Model {
            index,
            signals,
            weights: Weights::default(),
        }));
        let status = Arc::new(Mutex::new(AnalysisStatus {
            running: false,
            done,
            total,
            failed: 0,
        }));
        let enabled = Arc::new(AtomicBool::new(enabled));
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let worker = Worker {
            store: Store::open(db)?,
            model: model.clone(),
            status: status.clone(),
            enabled: enabled.clone(),
            events: ev_tx,
            failed: HashSet::new(),
            run: None,
            dirty: false,
            paused: Arc::new(AtomicBool::new(!enabled.load(Ordering::Acquire))),
        };
        let thread = std::thread::Builder::new()
            .name("lumen-reco".into())
            .spawn(move || worker.run(cmd_rx))
            .map_err(|e| format!("cannot start the recommendation thread: {e}"))?;
        Ok(Self {
            commands: cmd_tx,
            events: ev_rx,
            model,
            status,
            enabled,
            store: Mutex::new(Store::open(db)?),
            thread: Some(thread),
        })
    }

    pub fn events(&self) -> &Receiver<RecoEvent> {
        &self.events
    }

    pub fn status(&self) -> AnalysisStatus {
        *self.status.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn model(&self) -> RwLockReadGuard<'_, Model> {
        self.model.read().unwrap_or_else(|p| p.into_inner())
    }

    fn send(&self, c: Cmd) {
        let _ = self.commands.send(c);
    }

    /// Analyze new and changed tracks (after a scan or a file change).
    pub fn refresh(&self) {
        self.send(Cmd::Refresh);
    }

    /// A full library rescan: also retry files that failed before.
    pub fn rescan(&self) {
        self.send(Cmd::Rescan);
    }

    /// "Re-analyze library".
    pub fn reanalyze(&self) {
        self.send(Cmd::Reanalyze);
    }

    /// "On-device recommendations": off pauses analysis and stops recording plays.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Release);
        self.send(Cmd::SetEnabled(on));
    }

    pub fn record(&self, play: PlayRecord) {
        if self.enabled.load(Ordering::Acquire) {
            self.send(Cmd::Record(play));
        }
    }

    /// "Reset listening history": play events, co-play counts and saved
    /// mixes are gone when this returns (the audio analysis stays).
    pub fn reset_history(&self) {
        let mut store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = store.reset_history() {
            tracing::warn!("{e}");
        }
        self.model
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .signals
            .clear();
    }

    /// Up to `count` tracks for `seeds`, never one in `exclude`.
    pub fn recommend(
        &self,
        snap: &Snapshot,
        seeds: &[TrackId],
        count: usize,
        exclude: &HashSet<TrackId>,
        now_ms: i64,
        skips_as_dislikes: bool,
    ) -> Vec<TrackId> {
        self.model().recommend(
            snap,
            &Query {
                seeds,
                count,
                exclude: Some(exclude),
                now_ms,
                skips_as_dislikes,
                calmness: None,
            },
        )
    }

    /// Both mixes: the saved ones while under 6 hours old, else rebuilt
    /// (and saved). An empty mix is rebuilt every time, since it may have
    /// something to show now. `force` rebuilds regardless.
    pub fn mixes(&self, snap: &Snapshot, now_ms: i64, skips: bool, force: bool) -> Vec<Mix> {
        let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        MixKind::ALL
            .iter()
            .map(|&kind| {
                let saved = store.mix(kind.key()).unwrap_or_else(|e| {
                    tracing::warn!("{e}");
                    None
                });
                if let Some((at, tracks)) = saved {
                    let fresh = now_ms - at < MIX_EVERY_MS && now_ms >= at;
                    if fresh && !tracks.is_empty() && !force {
                        let tracks: Vec<TrackId> = tracks
                            .into_iter()
                            .filter(|t| snap.get(*t).is_some())
                            .collect();
                        return Mix {
                            kind,
                            generated_at: at,
                            tracks,
                        };
                    }
                }
                let started = Instant::now();
                let tracks = kind.build(&self.model(), snap, now_ms, skips);
                tracing::info!(
                    "{}: {} tracks built in {:?}",
                    kind.name(),
                    tracks.len(),
                    started.elapsed()
                );
                if let Err(e) = store.save_mix(kind.key(), now_ms, &tracks) {
                    tracing::warn!("{e}");
                }
                Mix {
                    kind,
                    generated_at: now_ms,
                    tracks,
                }
            })
            .collect()
    }
}

impl Drop for Reco {
    fn drop(&mut self) {
        self.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// An analysis run in progress.
struct Run {
    pool: Pool,
    results: Receiver<Done>,
    jobs: usize,
    finished: usize,
    analyzed: usize,
    failed: usize,
    started: Instant,
}

/// The recommendation thread.
struct Worker {
    store: Store,
    model: Arc<RwLock<Model>>,
    status: Arc<Mutex<AnalysisStatus>>,
    enabled: Arc<AtomicBool>,
    events: Sender<RecoEvent>,
    /// Failed this session; retried on the next rescan.
    failed: HashSet<TrackId>,
    run: Option<Run>,
    /// Tracks changed during a run: look again when it ends.
    dirty: bool,
    paused: Arc<AtomicBool>,
}

impl Worker {
    fn model(&self) -> RwLockWriteGuard<'_, Model> {
        self.model.write().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, e: RecoEvent) {
        let _ = self.events.send(e);
    }

    fn set_status(&self, f: impl FnOnce(&mut AnalysisStatus)) {
        let mut s = self.status.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut s);
        drop(s);
        self.emit(RecoEvent::Analysis);
    }

    fn run(mut self, commands: Receiver<Cmd>) {
        loop {
            let results = self.run.as_ref().map(|r| r.results.clone());
            let cmd = match &results {
                Some(results) => select! {
                    recv(commands) -> c => match c {
                        Ok(c) => Some(c),
                        Err(_) => break,
                    },
                    recv(results) -> d => {
                        if let Ok(d) = d {
                            self.on_done(d);
                        }
                        None
                    }
                },
                None => match commands.recv() {
                    Ok(c) => Some(c),
                    Err(_) => break,
                },
            };
            let Some(cmd) = cmd else { continue };
            match cmd {
                Cmd::Refresh => self.start_run(),
                Cmd::Rescan => {
                    self.failed.clear();
                    self.start_run();
                }
                Cmd::Reanalyze => {
                    if let Err(e) = self.store.request_reanalysis(lumen_library::db::now_ms()) {
                        tracing::warn!("{e}");
                    }
                    self.failed.clear();
                    self.start_run();
                }
                Cmd::SetEnabled(on) => {
                    self.paused.store(!on, Ordering::Release);
                    if on {
                        self.start_run();
                    } else {
                        self.set_status(|s| s.running = false);
                    }
                }
                Cmd::Record(play) => self.record(&play),
                Cmd::Shutdown => break,
            }
        }
        if let Some(run) = self.run.take() {
            run.pool.cancel();
        }
    }

    fn record(&mut self, play: &PlayRecord) {
        let hour = sys::local_hour(play.started_at);
        let applied = self.model().signals.record(play, hour);
        if let Err(e) = self.store.save_play(play, hour, &applied) {
            tracing::warn!("{e}");
        }
        self.emit(RecoEvent::ModelChanged);
    }

    /// Starts analyzing what is pending, unless a run is going (then once
    /// it ends) or recommendations are off.
    fn start_run(&mut self) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        if self.run.is_some() {
            self.dirty = true;
            return;
        }
        let pending = match self.store.pending() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("{e}");
                return;
            }
        };
        let (done, total) = self.store.analysis_counts().unwrap_or((0, 0));
        let jobs: Vec<_> = pending
            .into_iter()
            .filter(|p| !self.failed.contains(&p.id))
            .collect();
        let failed = self.failed.len();
        if jobs.is_empty() {
            self.set_status(|s| {
                *s = AnalysisStatus {
                    running: false,
                    done,
                    total,
                    failed,
                }
            });
            return;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let n = jobs.len();
        let workers = analyze::worker_count();
        tracing::info!("analyzing {n} tracks on {workers} threads");
        let pool = Pool::start(jobs, workers, self.paused.clone(), tx);
        if pool.is_empty() {
            return;
        }
        self.run = Some(Run {
            pool,
            results: rx,
            jobs: n,
            finished: 0,
            analyzed: 0,
            failed: 0,
            started: Instant::now(),
        });
        self.set_status(|s| {
            *s = AnalysisStatus {
                running: true,
                done,
                total,
                failed,
            }
        });
    }

    fn on_done(&mut self, d: Done) {
        let ok = match d.result {
            Ok(v) => match self
                .store
                .save_vector(&d.job, &v, lumen_library::db::now_ms())
            {
                Ok(()) => {
                    self.model().index.insert(d.job.id, v);
                    true
                }
                Err(e) => {
                    tracing::warn!("{e}");
                    false
                }
            },
            Err(reason) => {
                tracing::warn!("cannot analyze {}: {reason}", d.job.path.display());
                self.failed.insert(d.job.id);
                false
            }
        };
        let Some(run) = self.run.as_mut() else { return };
        run.finished += 1;
        if ok {
            run.analyzed += 1;
        } else {
            run.failed += 1;
        }
        let finished = run.finished >= run.jobs;
        let analyzed = run.analyzed;
        let failed_now = self.failed.len();
        self.set_status(|s| {
            if ok {
                s.done = (s.done + 1).min(s.total);
            }
            s.failed = failed_now;
            s.running = !finished;
        });
        if ok && (analyzed % 25 == 0 || finished) {
            self.emit(RecoEvent::ModelChanged);
        }
        if finished && let Some(run) = self.run.take() {
            let elapsed = run.started.elapsed();
            tracing::info!(
                "analysis finished: {} tracks in {:.1} s ({} failed)",
                run.analyzed,
                elapsed.as_secs_f64(),
                run.failed
            );
            run.pool.join();
            self.emit(RecoEvent::RunFinished {
                analyzed: run.analyzed,
                failed: run.failed,
                elapsed,
            });
            if std::mem::take(&mut self.dirty) {
                self.start_run();
            } else if let Ok((done, total)) = self.store.analysis_counts() {
                let failed = self.failed.len();
                self.set_status(|s| {
                    *s = AnalysisStatus {
                        running: false,
                        done,
                        total,
                        failed,
                    }
                });
            }
        }
    }
}

/// Where the library database lives for a data folder (for tools).
pub fn db_in(dir: &Path) -> PathBuf {
    lumen_library::LibraryPaths::in_dir(dir).db
}
