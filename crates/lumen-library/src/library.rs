//! The library thread: owns the writer connection, runs scans, applies
//! folder-watcher events and reports [`LibraryEvent`]s.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use lumen_core::{LyricsFlags, TrackId, is_audio_path};
use notify::RecursiveMode;
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify_debouncer_full::{
    DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache, new_debouncer,
};

use crate::db::{Db, Existing, Folder, Reader, now_ms};
use crate::paths::{LibraryPaths, is_within};
use crate::read::{FileStat, ScannedTrack, ThumbCache, lrc_flags, read_track, stat};

/// LRC-related flag bits (refreshed when only the `.lrc` changes).
const LRC_BITS: u8 = LyricsFlags::LRC | LyricsFlags::LRC_SYNCED | LyricsFlags::LRC_ENHANCED;
/// Tracks per database transaction while scanning.
const BATCH: usize = 64;
/// Maximum delay before scanned tracks are committed (and shown).
const COMMIT_INTERVAL: Duration = Duration::from_millis(250);
/// Progress events at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Library settings.
#[derive(Clone, Debug)]
pub struct LibraryConfig {
    pub paths: LibraryPaths,
    /// Watch folders for changes.
    pub watch: bool,
    /// Run an incremental scan when the library opens (picks up changes made
    /// while Lumen was closed).
    pub scan_on_open: bool,
    /// Tag-reading threads.
    pub workers: usize,
    /// How long the watcher waits for a burst of file changes to settle.
    pub debounce: Duration,
    /// Update "Date modified" when a file changes.
    pub track_mtime: bool,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            paths: LibraryPaths::default_location(),
            watch: true,
            scan_on_open: true,
            workers: std::thread::available_parallelism()
                .map(|n| n.get().clamp(2, 8))
                .unwrap_or(4),
            debounce: Duration::from_millis(1000),
            track_mtime: true,
        }
    }
}

/// Events from the library thread.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryEvent {
    ScanStarted,
    /// `done` of `total` files read (only files that are new or changed).
    ScanProgress {
        done: usize,
        total: usize,
    },
    ScanFinished {
        added: usize,
        updated: usize,
        removed: usize,
        elapsed: Duration,
    },
    /// Tracks, albums or artists changed: re-query what is on screen.
    Changed,
    FoldersChanged,
    /// A playlist was created (new, or imported from `.m3u8`).
    PlaylistCreated {
        id: i64,
        /// For imports: entries matched and entries not in the library.
        added: usize,
        missing: usize,
    },
    Error(String),
}

enum Cmd {
    AddFolder(PathBuf),
    RemoveFolder(i64),
    Rescan,
    SetWatch(bool),
    RecordPlay(TrackId),
    ResetPlayCounts,
    SetLyricsOffset(TrackId, i32),
    AddToPlaylist(i64, Vec<TrackId>),
    /// Creates a playlist holding these tracks.
    NewPlaylist(String, Vec<TrackId>),
    RenamePlaylist(i64, String),
    DeletePlaylist(i64),
    MoveInPlaylist(i64, usize, usize),
    RemoveFromPlaylist(i64, usize),
    ImportPlaylist(PathBuf),
    SetTrackMtime(bool),
    Shutdown,
}

/// Handle to the library. Cheap calls; all work happens on its thread.
pub struct Library {
    commands: Sender<Cmd>,
    events: Receiver<LibraryEvent>,
    paths: LibraryPaths,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    set_aside: Option<PathBuf>,
}

impl Library {
    /// Opens (creating or migrating) the database and starts the library thread.
    pub fn open(config: LibraryConfig) -> Result<Self, String> {
        let db = Db::open(&config.paths.db)?;
        let set_aside = db.set_aside.clone();
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let paths = config.paths.clone();
        let thread = std::thread::Builder::new()
            .name("lumen-library".into())
            .spawn(move || {
                let panic_tx = ev_tx.clone();
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    Worker::new(db, config, ev_tx, thread_stop).run(cmd_rx);
                }));
                if let Err(p) = run {
                    let msg = p
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    tracing::error!("library thread panicked: {msg}");
                    let _ = panic_tx.send(LibraryEvent::Error(format!("library crashed: {msg}")));
                }
            })
            .map_err(|err| format!("cannot start the library thread: {err}"))?;
        Ok(Self {
            commands: cmd_tx,
            events: ev_rx,
            paths,
            stop,
            thread: Some(thread),
            set_aside,
        })
    }

    pub fn events(&self) -> &Receiver<LibraryEvent> {
        &self.events
    }

    /// A new read-only connection (one per thread that queries).
    pub fn reader(&self) -> Result<Reader, String> {
        Reader::open(&self.paths.db)
    }

    pub fn paths(&self) -> &LibraryPaths {
        &self.paths
    }

    /// Thumbnail file for a cover hash.
    pub fn thumb_path(&self, hash: &str) -> PathBuf {
        self.paths.thumb(hash)
    }

    pub fn add_folder(&self, path: impl Into<PathBuf>) {
        let _ = self.commands.send(Cmd::AddFolder(path.into()));
    }

    pub fn remove_folder(&self, id: i64) {
        let _ = self.commands.send(Cmd::RemoveFolder(id));
    }

    /// A database from another program that was found at the library path
    /// and renamed to this file so a new library could start.
    pub fn set_aside(&self) -> Option<&Path> {
        self.set_aside.as_deref()
    }

    pub fn rescan(&self) {
        let _ = self.commands.send(Cmd::Rescan);
    }

    pub fn set_watch(&self, on: bool) {
        let _ = self.commands.send(Cmd::SetWatch(on));
    }

    pub fn record_play(&self, id: TrackId) {
        let _ = self.commands.send(Cmd::RecordPlay(id));
    }

    pub fn reset_play_counts(&self) {
        let _ = self.commands.send(Cmd::ResetPlayCounts);
    }

    pub fn set_lyrics_offset(&self, id: TrackId, offset_ms: i32) {
        let _ = self.commands.send(Cmd::SetLyricsOffset(id, offset_ms));
    }

    /// Appends tracks to a playlist.
    pub fn add_to_playlist(&self, playlist: i64, tracks: Vec<TrackId>) {
        let _ = self.commands.send(Cmd::AddToPlaylist(playlist, tracks));
    }

    /// Creates a playlist named `name` with `tracks` in it.
    pub fn new_playlist(&self, name: impl Into<String>, tracks: Vec<TrackId>) {
        let _ = self.commands.send(Cmd::NewPlaylist(name.into(), tracks));
    }

    pub fn rename_playlist(&self, id: i64, name: impl Into<String>) {
        let _ = self.commands.send(Cmd::RenamePlaylist(id, name.into()));
    }

    pub fn delete_playlist(&self, id: i64) {
        let _ = self.commands.send(Cmd::DeletePlaylist(id));
    }

    /// Drag to reorder: entry `from` moves to index `to`.
    pub fn move_in_playlist(&self, id: i64, from: usize, to: usize) {
        let _ = self.commands.send(Cmd::MoveInPlaylist(id, from, to));
    }

    pub fn remove_from_playlist(&self, id: i64, index: usize) {
        let _ = self.commands.send(Cmd::RemoveFromPlaylist(id, index));
    }

    /// Imports an `.m3u`/`.m3u8` file as a new playlist named after it.
    pub fn import_playlist(&self, path: impl Into<PathBuf>) {
        let _ = self.commands.send(Cmd::ImportPlaylist(path.into()));
    }

    /// "Track file date modified".
    pub fn set_track_mtime(&self, on: bool) {
        let _ = self.commands.send(Cmd::SetTrackMtime(on));
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.commands.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One file to (re)read.
struct Job {
    folder_id: i64,
    path: PathBuf,
    stat: FileStat,
}

#[derive(Default)]
struct Counts {
    added: usize,
    updated: usize,
    removed: usize,
}

type Watcher = Debouncer<notify::RecommendedWatcher, RecommendedCache>;

struct Worker {
    db: Db,
    config: LibraryConfig,
    events: Sender<LibraryEvent>,
    stop: Arc<AtomicBool>,
    thumbs: Arc<ThumbCache>,
    watcher: Option<Watcher>,
    watch_tx: Sender<DebounceEventResult>,
    watch_rx: Receiver<DebounceEventResult>,
}

impl Worker {
    fn new(
        mut db: Db,
        config: LibraryConfig,
        events: Sender<LibraryEvent>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        db.track_mtime = config.track_mtime;
        let (watch_tx, watch_rx) = crossbeam_channel::unbounded();
        Self {
            thumbs: Arc::new(ThumbCache::new(config.paths.thumbs.clone())),
            db,
            config,
            events,
            stop,
            watcher: None,
            watch_tx,
            watch_rx,
        }
    }

    fn import_playlist(&mut self, path: &Path) -> Result<(), String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        let base = path.parent().unwrap_or(Path::new(""));
        let mut ids = Vec::new();
        let mut missing = 0;
        for p in crate::m3u::parse(&text, base) {
            match self.db.track_id_by_path(&p)? {
                Some(id) => ids.push(id),
                None => missing += 1,
            }
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Imported playlist".into());
        let id = self.db.create_playlist(&name)?;
        self.db.add_to_playlist(id, &ids)?;
        self.emit(LibraryEvent::Changed);
        self.emit(LibraryEvent::PlaylistCreated {
            id,
            added: ids.len(),
            missing,
        });
        Ok(())
    }

    fn emit(&self, e: LibraryEvent) {
        let _ = self.events.send(e);
    }

    fn error(&self, msg: String) {
        tracing::warn!("{msg}");
        self.emit(LibraryEvent::Error(msg));
    }

    fn run(mut self, commands: Receiver<Cmd>) {
        if self.config.watch {
            self.start_watching();
        }
        if self.config.scan_on_open {
            self.scan_all();
        }
        loop {
            let watch_rx = self.watch_rx.clone();
            select! {
                recv(commands) -> cmd => match cmd {
                    Ok(Cmd::Shutdown) | Err(_) => break,
                    Ok(cmd) => self.handle(cmd),
                },
                recv(watch_rx) -> res => match res {
                    Ok(Ok(events)) => self.apply_fs_events(events),
                    Ok(Err(errors)) => {
                        for e in errors {
                            tracing::warn!("folder watcher: {e}");
                        }
                    }
                    Err(_) => {}
                },
            }
            if self.stop.load(Ordering::Acquire) {
                break;
            }
        }
        if let Some(w) = self.watcher.take() {
            w.stop();
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        let result = match cmd {
            Cmd::AddFolder(path) => self.add_folder(&path),
            Cmd::RemoveFolder(id) => {
                let folder = self
                    .db
                    .folders()
                    .ok()
                    .and_then(|f| f.into_iter().find(|f| f.id == id));
                let r = self.db.remove_folder(id);
                if let (Some(w), Some(f)) = (self.watcher.as_mut(), folder) {
                    let _ = w.unwatch(&f.path);
                }
                self.emit(LibraryEvent::FoldersChanged);
                self.emit(LibraryEvent::Changed);
                r
            }
            Cmd::Rescan => {
                self.scan_all();
                Ok(())
            }
            Cmd::SetWatch(on) => {
                if on && self.watcher.is_none() {
                    self.start_watching();
                    self.scan_all();
                } else if !on && let Some(w) = self.watcher.take() {
                    w.stop();
                }
                Ok(())
            }
            Cmd::RecordPlay(id) => self
                .db
                .record_play(id)
                .map(|_| self.emit(LibraryEvent::Changed)),
            Cmd::ResetPlayCounts => self
                .db
                .reset_play_counts()
                .map(|_| self.emit(LibraryEvent::Changed)),
            Cmd::SetLyricsOffset(id, ms) => self
                .db
                .set_lyrics_offset(id, ms)
                .map(|_| self.emit(LibraryEvent::Changed)),
            Cmd::AddToPlaylist(id, tracks) => self
                .db
                .add_to_playlist(id, &tracks)
                .map(|_| self.emit(LibraryEvent::Changed)),
            Cmd::NewPlaylist(name, tracks) => self
                .db
                .create_playlist(&name)
                .and_then(|id| self.db.add_to_playlist(id, &tracks).map(|_| id))
                .map(|id| {
                    self.emit(LibraryEvent::Changed);
                    self.emit(LibraryEvent::PlaylistCreated {
                        id,
                        added: tracks.len(),
                        missing: 0,
                    });
                }),
            Cmd::RenamePlaylist(id, name) => self
                .db
                .rename_playlist(id, &name)
                .map(|_| self.emit(LibraryEvent::Changed)),
            Cmd::DeletePlaylist(id) => self
                .db
                .delete_playlist(id)
                .map(|_| self.emit(LibraryEvent::Changed)),
            Cmd::MoveInPlaylist(id, from, to) => self
                .db
                .move_in_playlist(id, from, to)
                .map(|_| self.emit(LibraryEvent::Changed)),
            Cmd::RemoveFromPlaylist(id, index) => self
                .db
                .remove_from_playlist(id, index)
                .map(|_| self.emit(LibraryEvent::Changed)),
            Cmd::ImportPlaylist(path) => self.import_playlist(&path),
            Cmd::SetTrackMtime(on) => {
                self.db.track_mtime = on;
                Ok(())
            }
            Cmd::Shutdown => Ok(()),
        };
        if let Err(e) = result {
            self.error(e);
        }
    }

    fn add_folder(&mut self, path: &Path) -> Result<(), String> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if !path.is_dir() {
            return Err(format!("{} is not a folder", path.display()));
        }
        let id = self.db.add_folder(&path)?;
        self.emit(LibraryEvent::FoldersChanged);
        if let Some(w) = self.watcher.as_mut()
            && let Err(e) = w.watch(&path, RecursiveMode::Recursive)
        {
            tracing::warn!("cannot watch {}: {e}", path.display());
        }
        let folder = Folder {
            id,
            path: path.clone(),
            added_at: now_ms(),
        };
        self.scan(&[(folder, path)], true);
        Ok(())
    }

    fn start_watching(&mut self) {
        let tx = self.watch_tx.clone();
        let debouncer = new_debouncer(
            self.config.debounce,
            None,
            move |res: DebounceEventResult| {
                let _ = tx.send(res);
            },
        );
        match debouncer {
            Ok(mut w) => {
                for f in self.db.folders().unwrap_or_default() {
                    if f.path.is_dir()
                        && let Err(e) = w.watch(&f.path, RecursiveMode::Recursive)
                    {
                        tracing::warn!("cannot watch {}: {e}", f.path.display());
                    }
                }
                self.watcher = Some(w);
            }
            Err(e) => self.error(format!("folder watching unavailable: {e}")),
        }
    }

    // ---- scanning ------------------------------------------------------------

    fn scan_all(&mut self) {
        match self.db.folders() {
            Ok(folders) => {
                let roots: Vec<(Folder, PathBuf)> = folders
                    .into_iter()
                    .map(|f| {
                        let p = f.path.clone();
                        (f, p)
                    })
                    .collect();
                self.scan(&roots, true);
            }
            Err(e) => self.error(e),
        }
    }

    /// Incremental scan of `roots` (a folder and a directory inside it).
    /// With `announce`, reports progress and records the scan time.
    fn scan(&mut self, roots: &[(Folder, PathBuf)], announce: bool) -> Counts {
        let started = Instant::now();
        if announce {
            self.emit(LibraryEvent::ScanStarted);
        }
        let mut counts = Counts::default();
        let mut jobs: Vec<Job> = Vec::new();
        let mut missing: Vec<(i64, Existing, i64)> = Vec::new(); // (id, existing, folder)
        let mut lrc_refresh: Vec<(i64, PathBuf, u8, Option<i64>)> = Vec::new();

        for (folder, root) in roots {
            if !root.is_dir() {
                // Offline drive or deleted folder: keep its tracks (and their
                // play counts and playlists) until it comes back.
                tracing::warn!("{} is not available; skipping", root.display());
                continue;
            }
            let existing = match self.db.existing_in_folder(folder.id) {
                Ok(m) => m,
                Err(e) => {
                    self.error(e);
                    continue;
                }
            };
            let mut present = HashSet::new();
            for entry in walkdir::WalkDir::new(root).follow_links(true).into_iter() {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!("cannot read {e}");
                        continue;
                    }
                };
                if !entry.file_type().is_file() || !is_audio_path(entry.path()) {
                    continue;
                }
                let path = entry.into_path();
                let Ok(st) = stat(&path) else { continue };
                match existing.get(&path.to_string_lossy().to_lowercase()) {
                    Some(ex) if ex.size == st.size && ex.mtime_ms == st.mtime_ms => {
                        present.insert(ex.id);
                        let lrc_now = crate::read::find_lrc(&path)
                            .and_then(|l| stat(&l).ok())
                            .map(|s| s.mtime_ms);
                        if lrc_now != ex.lrc_mtime {
                            lrc_refresh.push((ex.id, path, ex.lyrics, ex.lrc_mtime));
                        }
                    }
                    Some(ex) => {
                        present.insert(ex.id);
                        jobs.push(Job {
                            folder_id: folder.id,
                            path,
                            stat: st,
                        });
                    }
                    None => jobs.push(Job {
                        folder_id: folder.id,
                        path,
                        stat: st,
                    }),
                }
            }
            for (path, ex) in &existing {
                if !present.contains(&ex.id) && is_within(Path::new(path), root) {
                    missing.push((ex.id, *ex, folder.id));
                }
            }
        }

        // Moves and renames: a missing track and a new file with the same
        // size and mtime are the same file. Keep its id and history.
        if !missing.is_empty() {
            let mut by_stat: HashMap<(u64, i64), Vec<usize>> = HashMap::new();
            for (i, j) in jobs.iter().enumerate() {
                if self.db.existing(&j.path).ok().flatten().is_none() {
                    by_stat
                        .entry((j.stat.size, j.stat.mtime_ms))
                        .or_default()
                        .push(i);
                }
            }
            let mut moved_jobs = HashSet::new();
            missing.retain(|(id, ex, _)| {
                let Some(candidates) = by_stat.get_mut(&(ex.size, ex.mtime_ms)) else {
                    return true;
                };
                let Some(i) = candidates.pop() else {
                    return true;
                };
                let job = &jobs[i];
                match self.db.move_track(*id, &job.path, job.folder_id) {
                    Ok(()) => {
                        moved_jobs.insert(i);
                        counts.updated += 1;
                        false
                    }
                    Err(_) => true,
                }
            });
            let mut i = 0;
            jobs.retain(|_| {
                let keep = !moved_jobs.contains(&i);
                i += 1;
                keep
            });
        }

        let total = jobs.len();
        if announce {
            self.emit(LibraryEvent::ScanProgress { done: 0, total });
        }
        self.read_jobs(jobs, announce, &mut counts);

        for (id, path, old, _) in lrc_refresh {
            let (flags, mtime) = lrc_flags(&path);
            let _ = self
                .db
                .update_lyrics(id, (old & !LRC_BITS) | flags.0, mtime);
        }
        let ids: Vec<i64> = missing.iter().map(|(id, _, _)| *id).collect();
        match self.db.delete_tracks(&ids) {
            Ok(n) => counts.removed += n,
            Err(e) => self.error(e),
        }

        if announce {
            let _ = self.db.set_meta("last_scan", &now_ms().to_string());
            self.emit(LibraryEvent::ScanFinished {
                added: counts.added,
                updated: counts.updated,
                removed: counts.removed,
                elapsed: started.elapsed(),
            });
            tracing::info!(
                "scan: {} added, {} updated, {} removed in {:?}",
                counts.added,
                counts.updated,
                counts.removed,
                started.elapsed()
            );
        }
        if counts.added + counts.updated + counts.removed > 0 || announce {
            self.emit(LibraryEvent::Changed);
        }
        counts
    }

    /// Reads tags on a thread pool and commits results in batches as they arrive.
    fn read_jobs(&mut self, jobs: Vec<Job>, announce: bool, counts: &mut Counts) {
        let total = jobs.len();
        if total == 0 {
            return;
        }
        let workers = self.config.workers.max(1).min(total);
        let (job_tx, job_rx) = crossbeam_channel::bounded::<Job>(workers * 4);
        let (res_tx, res_rx) =
            crossbeam_channel::unbounded::<(i64, PathBuf, Result<ScannedTrack, String>)>();
        let stop = self.stop.clone();
        let thumbs = self.thumbs.clone();

        std::thread::scope(|s| {
            for n in 0..workers {
                let job_rx = job_rx.clone();
                let res_tx = res_tx.clone();
                let thumbs = thumbs.clone();
                let stop = stop.clone();
                let _ = std::thread::Builder::new()
                    .name(format!("lumen-tags-{n}"))
                    .spawn_scoped(s, move || {
                        for job in job_rx {
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                read_track(&job.path, job.stat, &thumbs)
                            }))
                            .unwrap_or_else(|_| Err("tag reader panicked".into()));
                            if res_tx.send((job.folder_id, job.path, r)).is_err() {
                                break;
                            }
                        }
                    });
            }
            drop(res_tx);
            let feeder_stop = stop.clone();
            let _ = std::thread::Builder::new()
                .name("lumen-scan-feed".into())
                .spawn_scoped(s, move || {
                    for job in jobs {
                        if feeder_stop.load(Ordering::Relaxed) || job_tx.send(job).is_err() {
                            break;
                        }
                    }
                });
            drop(job_rx);

            let mut done = 0;
            let mut batch: HashMap<i64, Vec<ScannedTrack>> = HashMap::new();
            let mut pending = 0;
            let mut last_commit = Instant::now();
            let mut last_progress = Instant::now();
            let commit =
                |batch: &mut HashMap<i64, Vec<ScannedTrack>>, db: &mut Db, counts: &mut Counts| {
                    for (folder_id, tracks) in batch.drain() {
                        match db.write_tracks(folder_id, &tracks) {
                            Ok((a, u)) => {
                                counts.added += a;
                                counts.updated += u;
                            }
                            Err(e) => tracing::error!("{e}"),
                        }
                    }
                };
            for (folder_id, path, result) in res_rx {
                done += 1;
                match result {
                    Ok(t) => {
                        batch.entry(folder_id).or_default().push(t);
                        pending += 1;
                    }
                    Err(e) => tracing::warn!("skipping {}: {e}", path.display()),
                }
                if pending >= BATCH || (pending > 0 && last_commit.elapsed() >= COMMIT_INTERVAL) {
                    commit(&mut batch, &mut self.db, counts);
                    pending = 0;
                    last_commit = Instant::now();
                    // Progress first, so the counter never trails the committed tracks.
                    if announce {
                        last_progress = Instant::now();
                        let _ = self.events.send(LibraryEvent::ScanProgress { done, total });
                    }
                    let _ = self.events.send(LibraryEvent::Changed);
                    continue;
                }
                if announce && (last_progress.elapsed() >= PROGRESS_INTERVAL || done == total) {
                    last_progress = Instant::now();
                    let _ = self.events.send(LibraryEvent::ScanProgress { done, total });
                }
            }
            commit(&mut batch, &mut self.db, counts);
        });
    }

    // ---- watching ------------------------------------------------------------

    fn folder_for(&self, path: &Path) -> Option<Folder> {
        self.db
            .folders()
            .ok()?
            .into_iter()
            .find(|f| is_within(path, &f.path))
    }

    fn apply_fs_events(&mut self, events: Vec<DebouncedEvent>) {
        let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut dirty: Vec<PathBuf> = Vec::new();
        for ev in events {
            match ev.kind {
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
                    renames.push((ev.paths[0].clone(), ev.paths[1].clone()));
                }
                EventKind::Access(_) => {}
                _ => dirty.extend(ev.paths.iter().cloned()),
            }
        }
        let mut changed = false;
        for (from, to) in renames {
            changed |= self.apply_rename(&from, &to);
        }
        let mut seen = HashSet::new();
        for p in dirty {
            if seen.insert(p.to_string_lossy().to_lowercase()) {
                changed |= self.reconcile(&p);
            }
        }
        if changed {
            self.emit(LibraryEvent::Changed);
        }
    }

    /// Brings the library in line with whatever is at `path` now.
    fn reconcile(&mut self, path: &Path) -> bool {
        let Some(folder) = self.folder_for(path) else {
            return false;
        };
        if is_lrc(path) {
            return self.refresh_lyrics_for(path, &folder);
        }
        if path.is_dir() {
            let c = self.scan(&[(folder, path.to_path_buf())], false);
            return c.added + c.updated + c.removed > 0;
        }
        if path.is_file() {
            if !is_audio_path(path) {
                return false;
            }
            let Ok(st) = stat(path) else { return false };
            if let Ok(Some(ex)) = self.db.existing(path)
                && ex.size == st.size
                && ex.mtime_ms == st.mtime_ms
            {
                return false;
            }
            return match read_track(path, st, &self.thumbs) {
                Ok(t) => self.db.write_tracks(folder.id, &[t]).is_ok(),
                Err(e) => {
                    // Often a file still being copied; the next change event retries.
                    tracing::warn!("cannot read {} yet: {e}", path.display());
                    false
                }
            };
        }
        // Gone: a file, or a whole directory.
        let ids: Vec<i64> = self
            .db
            .tracks_under(folder.id, path)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        matches!(self.db.delete_tracks(&ids), Ok(n) if n > 0)
    }

    fn apply_rename(&mut self, from: &Path, to: &Path) -> bool {
        let Some(to_folder) = self.folder_for(to) else {
            return self.reconcile(from);
        };
        if is_lrc(from) || is_lrc(to) {
            let a = self
                .folder_for(from)
                .is_some_and(|f| self.refresh_lyrics_for(from, &f));
            let b = self.refresh_lyrics_for(to, &to_folder);
            return a | b;
        }
        let mut changed = false;
        if let Some(from_folder) = self.folder_for(from) {
            let under = self
                .db
                .tracks_under(from_folder.id, from)
                .unwrap_or_default();
            let from_str = from.to_string_lossy().len();
            for (id, old) in under {
                // `old` is `from` itself (file rename) or a file inside it (dir rename).
                let suffix = old.to_string_lossy()[from_str..].to_string();
                let new = PathBuf::from(format!("{}{suffix}", to.to_string_lossy()));
                if is_audio_path(&new) {
                    changed |= self.db.move_track(id, &new, to_folder.id).is_ok();
                }
            }
        }
        changed | self.reconcile(to)
    }

    /// Re-reads `.lrc` presence for audio files sharing `lrc`'s name.
    fn refresh_lyrics_for(&mut self, lrc: &Path, folder: &Folder) -> bool {
        let Some(dir) = lrc.parent() else {
            return false;
        };
        let stem = lrc.with_extension("").to_string_lossy().to_lowercase();
        let tracks = self.db.tracks_under(folder.id, dir).unwrap_or_default();
        let mut changed = false;
        for (id, path) in tracks {
            if path.with_extension("").to_string_lossy().to_lowercase() != stem {
                continue;
            }
            let Ok(Some(ex)) = self.db.existing(&path) else {
                continue;
            };
            let (flags, mtime) = lrc_flags(&path);
            let new = (ex.lyrics & !LRC_BITS) | flags.0;
            if new != ex.lyrics || mtime != ex.lrc_mtime {
                changed |= self.db.update_lyrics(id, new, mtime).is_ok();
            }
        }
        changed
    }
}

fn is_lrc(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("lrc"))
}
