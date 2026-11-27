//! User settings, saved to `%APPDATA%\Lumen\settings.toml`.
//!
//! Unknown keys are ignored and missing keys take their defaults, so older and
//! newer files both load.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sort::{AlbumSort, ArtistSort, PlaylistSort, SongSort, SortDir};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub audio: AudioSettings,
    pub library: LibrarySettings,
    pub lyrics: LyricsSettings,
}

/// Settings › Audio output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// WASAPI endpoint id; `None` = system default device.
    pub device_id: Option<String>,
    /// Name of that device, shown when it is not plugged in.
    pub device_name: Option<String>,
    /// Fixed output rate in Hz; `None` = match each file (bit-perfect).
    pub sample_rate: Option<u32>,
    pub exclusive: bool,
    pub gapless: bool,
    pub replaygain: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            device_id: None,
            device_name: None,
            sample_rate: None,
            exclusive: true,
            gapless: true,
            replaygain: false,
        }
    }
}

impl AudioSettings {
    /// Sample rate choices: `None` is "Match source file (bit-perfect)".
    pub const RATES: [Option<u32>; 4] = [None, Some(44_100), Some(48_000), Some(96_000)];

    pub fn rate_label(rate: Option<u32>) -> &'static str {
        match rate {
            None => "Match source file (bit-perfect)",
            Some(44_100) => "44.1 kHz",
            Some(48_000) => "48 kHz",
            Some(96_000) => "96 kHz",
            Some(_) => "Custom",
        }
    }
}

/// Font for lyrics. Every choice keeps the bundled Noto Devanagari and
/// Malayalam fallbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsFont {
    #[default]
    SpaceGrotesk,
    SegoeUi,
    NotoSans,
}

impl LyricsFont {
    pub const ALL: [LyricsFont; 3] = [
        LyricsFont::SpaceGrotesk,
        LyricsFont::SegoeUi,
        LyricsFont::NotoSans,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LyricsFont::SpaceGrotesk => "Space Grotesk + Noto Sans fallback",
            LyricsFont::SegoeUi => "Segoe UI",
            LyricsFont::NotoSans => "Noto Sans",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsSettings {
    /// Load `song.lrc` next to `song.flac`.
    pub load_lrc: bool,
    /// Use SYLT / USLT / LYRICS tags.
    pub embedded: bool,
    /// Fill words progressively for enhanced LRC.
    pub word_highlight: bool,
    pub font: LyricsFont,
    /// Full-screen lyrics size (A− / A+), 0.7 to 1.3.
    pub fullscreen_scale: f32,
}

impl Default for LyricsSettings {
    fn default() -> Self {
        Self {
            load_lrc: true,
            embedded: true,
            word_highlight: true,
            font: LyricsFont::SpaceGrotesk,
            fullscreen_scale: 1.0,
        }
    }
}

impl LyricsSettings {
    pub const SCALE_MIN: f32 = 0.7;
    pub const SCALE_MAX: f32 = 1.3;
    pub const SCALE_STEP: f32 = 0.1;

    /// A− / A+: one step, kept on the 0.1 grid and within range.
    pub fn step_scale(&mut self, up: bool) {
        let s = self.fullscreen_scale
            + if up {
                Self::SCALE_STEP
            } else {
                -Self::SCALE_STEP
            };
        self.fullscreen_scale = ((s * 10.0).round() / 10.0).clamp(Self::SCALE_MIN, Self::SCALE_MAX);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibrarySettings {
    /// Songs screen sort. Settings › Library "Default song sort" edits the
    /// same value, so the choice persists either way.
    pub song_sort: SongSort,
    pub song_sort_dir: SortDir,
    pub album_sort: AlbumSort,
    pub album_sort_dir: SortDir,
    pub artist_sort: ArtistSort,
    pub artist_sort_dir: SortDir,
    pub playlist_sort: PlaylistSort,
    pub playlist_sort_dir: SortDir,
    /// "Watch folders for changes".
    pub watch: bool,
    /// "Track file date modified": Date modified follows file changes.
    pub track_mtime: bool,
}

impl Default for LibrarySettings {
    fn default() -> Self {
        Self {
            // The spec's default: date modified, newest first.
            song_sort: SongSort::DateModified,
            song_sort_dir: SortDir::Desc,
            album_sort: AlbumSort::Title,
            album_sort_dir: SortDir::Asc,
            artist_sort: ArtistSort::Name,
            artist_sort_dir: SortDir::Asc,
            playlist_sort: PlaylistSort::Custom,
            playlist_sort_dir: SortDir::Asc,
            watch: true,
            track_mtime: true,
        }
    }
}

impl Settings {
    /// `%APPDATA%\Lumen` (the temp dir if unset).
    pub fn default_dir() -> PathBuf {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Lumen")
    }

    pub fn file_in(dir: &Path) -> PathBuf {
        dir.join("settings.toml")
    }

    /// Loads `file`. A missing file gives the defaults; an unreadable one is
    /// kept aside as `settings.toml.bad` and the defaults are used.
    pub fn load(file: &Path) -> Self {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", file.display());
                return Self::default();
            }
        };
        match toml::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("{} is not valid, using defaults: {e}", file.display());
                let _ = std::fs::rename(file, file.with_extension("toml.bad"));
                Self::default()
            }
        }
    }

    /// Writes `file` atomically (temp file, then rename).
    pub fn save(&self, file: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, file).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_partial_files() {
        let dir = std::env::temp_dir().join(format!("lumen-settings-{}", std::process::id()));
        let file = Settings::file_in(&dir);
        let _ = std::fs::remove_file(&file);
        assert_eq!(Settings::load(&file), Settings::default());
        assert_eq!(
            Settings::default().library.song_sort,
            SongSort::DateModified
        );
        assert_eq!(Settings::default().library.song_sort_dir, SortDir::Desc);

        let mut s = Settings::default();
        s.library.song_sort = SongSort::PlayCount;
        s.library.artist_sort_dir = SortDir::Desc;
        s.save(&file).expect("save");
        assert_eq!(Settings::load(&file), s);

        std::fs::write(&file, "[library]\nsong_sort = \"artist\"\nfuture_key = 1\n").expect("w");
        let partial = Settings::load(&file);
        assert_eq!(partial.library.song_sort, SongSort::Artist);
        assert_eq!(partial.library.song_sort_dir, SortDir::Desc);

        let mut l = LyricsSettings::default();
        for _ in 0..10 {
            l.step_scale(true);
        }
        assert_eq!(l.fullscreen_scale, 1.3);
        for _ in 0..10 {
            l.step_scale(false);
        }
        assert_eq!(l.fullscreen_scale, 0.7);
        s.lyrics = l;
        s.audio.device_id = Some("{0.0.0.00000000}.{abc}".into());
        s.audio.sample_rate = Some(48_000);
        s.audio.exclusive = false;
        s.library.watch = false;
        s.library.playlist_sort = PlaylistSort::Duration;
        s.lyrics.font = LyricsFont::NotoSans;
        s.save(&file).expect("save");
        assert_eq!(Settings::load(&file), s);

        std::fs::write(&file, "not toml [[[").expect("w");
        assert_eq!(Settings::load(&file), Settings::default());
        assert!(file.with_extension("toml.bad").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
