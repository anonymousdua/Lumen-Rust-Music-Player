//! The UI's view of the library: live stats, folders, scan progress and the
//! in-memory catalog the library screens sort and filter.

use std::path::PathBuf;
use std::sync::Arc;

use lumen_library::{
    Catalog, FolderInfo, Library, LibraryConfig, LibraryEvent, LibraryPaths, LibraryStats,
    PlaylistInfo, Reader,
};

pub struct LibraryModel {
    library: Option<Arc<Library>>,
    reader: Option<Reader>,
    pub paths: LibraryPaths,
    pub stats: LibraryStats,
    pub folders: Vec<FolderInfo>,
    pub playlists: Vec<PlaylistInfo>,
    pub catalog: Arc<Catalog>,
    /// `Some((done, total))` while scanning; total 0 = still walking folders.
    pub scan: Option<(usize, usize)>,
    /// Last error from the library (shown in Settings), cleared on success.
    pub last_error: Option<String>,
    /// The library could not be opened at all.
    pub open_error: Option<String>,
    /// One-time message for the user (an old database was set aside).
    pub notice: Option<String>,
    /// A catalog reload is running / another one is needed after it.
    pub(crate) loading: bool,
    pub(crate) stale: bool,
}

/// What a batch of library events asks the app to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Stats, folders or scan progress changed: redraw.
    pub redraw: bool,
    /// Tracks changed: reload the catalog.
    pub reload: bool,
}

impl LibraryModel {
    /// Opens `%LOCALAPPDATA%\Lumen\library.db` (or `LUMEN_DATA_DIR`) and
    /// starts the library thread, which scans for changes made while Lumen
    /// was closed. The catalog is read right away so the first frame is full.
    pub fn open(settings: &lumen_core::LibrarySettings) -> Self {
        let mut config = LibraryConfig {
            watch: settings.watch,
            track_mtime: settings.track_mtime,
            ..LibraryConfig::default()
        };
        // Testing and portable use: keep the database and thumbnails elsewhere.
        if let Some(dir) = std::env::var_os("LUMEN_DATA_DIR") {
            config.paths = LibraryPaths::in_dir(std::path::Path::new(&dir));
        }
        let mut model = Self {
            library: None,
            reader: None,
            paths: config.paths.clone(),
            stats: LibraryStats::default(),
            folders: Vec::new(),
            playlists: Vec::new(),
            catalog: Arc::new(Catalog::default()),
            scan: None,
            last_error: None,
            open_error: None,
            notice: None,
            loading: false,
            stale: false,
        };
        match Library::open(config) {
            Ok(lib) => {
                match lib.reader() {
                    Ok(r) => model.reader = Some(r),
                    Err(e) => model.open_error = Some(e),
                }
                if let Some(old) = lib.set_aside() {
                    model.notice = Some(format!(
                        "Found an older library file that isn't from this version of Lumen. It was kept as {} and a new library was started.",
                        old.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ));
                }
                model.library = Some(Arc::new(lib));
                model.refresh();
                if let Some(r) = &model.reader {
                    match r.catalog() {
                        Ok(c) => model.catalog = Arc::new(c),
                        Err(e) => tracing::warn!("cannot read the library: {e}"),
                    }
                }
            }
            Err(e) => {
                tracing::error!("library unavailable: {e}");
                model.open_error = Some(e);
            }
        }
        model
    }

    pub fn library(&self) -> Option<&Arc<Library>> {
        self.library.as_ref()
    }

    pub fn thumbs(&self) -> PathBuf {
        self.paths.thumbs.clone()
    }

    /// Re-reads stats, folders and playlists (a few milliseconds).
    pub fn refresh(&mut self) {
        let Some(r) = &self.reader else { return };
        match r.stats() {
            Ok(s) => self.stats = s,
            Err(e) => tracing::warn!("{e}"),
        }
        match r.folders() {
            Ok(f) => self.folders = f,
            Err(e) => tracing::warn!("{e}"),
        }
        match r.playlists() {
            Ok(p) => self.playlists = p,
            Err(e) => tracing::warn!("{e}"),
        }
    }

    /// Applies a batch of events.
    pub fn apply(&mut self, events: Vec<LibraryEvent>) -> Applied {
        let mut requery = false;
        for e in events {
            match e {
                LibraryEvent::ScanStarted => self.scan = Some((0, 0)),
                LibraryEvent::ScanProgress { done, total } => self.scan = Some((done, total)),
                LibraryEvent::ScanFinished { .. } => {
                    self.scan = None;
                    requery = true;
                }
                LibraryEvent::Changed | LibraryEvent::FoldersChanged => {
                    self.last_error = None;
                    requery = true;
                }
                LibraryEvent::Error(msg) => self.last_error = Some(msg),
                // The app picks this up to select the new playlist.
                LibraryEvent::PlaylistCreated { .. } => {}
            }
        }
        if requery {
            self.refresh();
        }
        Applied {
            redraw: true,
            reload: requery,
        }
    }

    /// A user playlist's entries in custom order.
    pub fn playlist_entries(&self, id: i64) -> Vec<lumen_library::PlaylistEntry> {
        self.reader
            .as_ref()
            .and_then(|r| r.playlist_entries(id).ok())
            .unwrap_or_default()
    }

    pub fn add_folder(&mut self, path: PathBuf) {
        if let Some(l) = &self.library {
            l.add_folder(path);
        }
    }

    pub fn remove_folder(&mut self, id: i64) {
        if let Some(l) = &self.library {
            l.remove_folder(id);
        }
    }

    pub fn rescan(&mut self) {
        if let Some(l) = &self.library {
            self.scan = Some((0, 0));
            l.rescan();
        }
    }
}
