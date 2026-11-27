//! Shared types for Lumen: track ids, tracks, playback modes, sort orders,
//! filters and settings.

use std::fmt;

pub mod resume;
pub mod settings;
pub mod sort;

pub use resume::ResumeState;
pub use settings::{
    AudioSettings, LibrarySettings, LyricsFont, LyricsSettings, RecoSettings, Settings,
};
pub use sort::{AlbumSort, ArtistSort, PlaylistSort, SongFilter, SongSort, SortDir};

/// Stable identifier of a track in the library database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub i64);

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Queue repeat mode.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    /// Stop after the last track.
    #[default]
    Off,
    /// Start over after the last track.
    All,
    /// Repeat the current track when it ends. Manual next/previous still move.
    One,
}

impl RepeatMode {
    /// Off → All → One → Off, the order the transport button cycles through.
    pub fn cycle(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }
}

/// How ReplayGain tags are applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReplayGainMode {
    #[default]
    Off,
    /// Per-track gain.
    Track,
    /// Per-album gain (falls back to track gain when a file has no album gain).
    Album,
}

/// Which lyrics a track has (bit flags, stored as an integer in the DB).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LyricsFlags(pub u8);

impl LyricsFlags {
    /// An `.lrc` file with the same name sits next to the audio file.
    pub const LRC: u8 = 1;
    /// The `.lrc` file has line timestamps.
    pub const LRC_SYNCED: u8 = 2;
    /// The `.lrc` file has word timestamps (`<mm:ss.xx>`).
    pub const LRC_ENHANCED: u8 = 4;
    /// Embedded unsynchronised lyrics (USLT, LYRICS without timestamps).
    pub const EMBEDDED: u8 = 8;
    /// Embedded synchronised lyrics (SYLT, or LYRICS with LRC timestamps).
    pub const EMBEDDED_SYNCED: u8 = 16;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub fn any(self) -> bool {
        self.0 != 0
    }

    pub fn synced(self) -> bool {
        self.has(Self::LRC_SYNCED) || self.has(Self::EMBEDDED_SYNCED)
    }
}

/// A track as stored in the library.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub path: std::path::PathBuf,
    pub title: String,
    pub artist: String,
    pub album_artist: Option<String>,
    pub album: String,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub duration_ms: u64,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    /// "FLAC", "MP3", "ALAC", "AAC", "WAV", "Vorbis", "Opus".
    pub codec: String,
    pub size: u64,
    /// File modification time (ms since the Unix epoch).
    pub mtime_ms: i64,
    /// First time Lumen saw the file (ms since the Unix epoch).
    pub date_added_ms: i64,
    /// Hash naming the cover thumbnail in the thumbnail cache.
    pub cover_hash: Option<String>,
    pub lyrics: LyricsFlags,
    pub lyrics_offset_ms: i32,
    pub play_count: u32,
    pub last_played_ms: Option<i64>,
}

impl Track {
    /// Lossless and at least 24-bit, or above 48 kHz.
    pub fn is_hires(&self) -> bool {
        self.bit_depth.is_some_and(|b| b >= 24) || self.sample_rate.is_some_and(|r| r > 48_000)
    }

    pub fn is_lossless(&self) -> bool {
        matches!(self.codec.as_str(), "FLAC" | "ALAC" | "WAV")
    }
}

/// File extensions (lowercase, without the dot) Lumen can play.
pub const AUDIO_EXTENSIONS: &[&str] = &["flac", "mp3", "m4a", "wav", "ogg", "oga"];

/// True if `path` has a playable audio extension (case-insensitive).
pub fn is_audio_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(e))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn repeat_cycles() {
        assert_eq!(RepeatMode::Off.cycle(), RepeatMode::All);
        assert_eq!(RepeatMode::All.cycle(), RepeatMode::One);
        assert_eq!(RepeatMode::One.cycle(), RepeatMode::Off);
    }

    #[test]
    fn audio_extensions() {
        assert!(is_audio_path(Path::new("a/b/Song.FLAC")));
        assert!(is_audio_path(Path::new("x.m4a")));
        assert!(!is_audio_path(Path::new("x.lrc")));
        assert!(!is_audio_path(Path::new("noext")));
    }
}
