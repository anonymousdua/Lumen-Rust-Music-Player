//! Lumen library: folder scanning, tags (lofty), cover thumbnails, SQLite
//! database (rusqlite, migrations), folder watching (notify), play counts,
//! lyric offsets and playlists.
//!
//! [`Library::open`] starts the library thread; call its methods to add
//! folders or rescan, read [`LibraryEvent`]s, and query through a
//! [`Reader`] (a read-only connection, one per querying thread).

pub mod catalog;
pub mod collate;
pub mod db;
pub mod library;
pub mod m3u;
pub mod paths;
pub mod read;
pub mod schema;
pub mod smart;

pub use catalog::{Album, Artist, CatTrack, Catalog, ListStats, fold};
pub use db::{FolderInfo, LibraryStats, PlaylistEntry, PlaylistInfo, Reader, TrackSort};
pub use library::{Library, LibraryConfig, LibraryEvent};
pub use paths::{LibraryPaths, data_dir};
pub use smart::SmartPlaylist;

/// The library's event channel.
pub type EventReceiver = crossbeam_channel::Receiver<LibraryEvent>;
