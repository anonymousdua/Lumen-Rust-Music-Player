//! What was playing when Lumen closed, restored (paused) on the next launch.
//! Saved to `%LOCALAPPDATA%\Lumen\playback.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::RepeatMode;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResumeState {
    /// Queue as library track ids, in queue order (not shuffled order).
    pub queue: Vec<i64>,
    /// Index into `queue` of the track that was playing.
    pub current: usize,
    pub position_ms: u64,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
}

impl Default for ResumeState {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            current: 0,
            position_ms: 0,
            volume: 1.0,
            shuffle: false,
            repeat: RepeatMode::Off,
        }
    }
}

impl ResumeState {
    pub fn file_in(dir: &Path) -> PathBuf {
        dir.join("playback.toml")
    }

    /// Missing or unreadable files give the default (nothing to resume).
    pub fn load(file: &Path) -> Self {
        match std::fs::read_to_string(file) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("ignoring {}: {e}", file.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes atomically (temp file, then rename).
    pub fn save(&self, file: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
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
    fn round_trips() {
        let dir = std::env::temp_dir().join(format!("lumen-resume-{}", std::process::id()));
        let file = ResumeState::file_in(&dir);
        assert_eq!(ResumeState::load(&file), ResumeState::default());
        let s = ResumeState {
            queue: vec![5, 9, 2],
            current: 1,
            position_ms: 83_250,
            volume: 0.4,
            shuffle: true,
            repeat: RepeatMode::One,
        };
        s.save(&file).expect("save");
        assert_eq!(ResumeState::load(&file), s);
        std::fs::write(&file, "queue = \"broken\"").expect("w");
        assert_eq!(ResumeState::load(&file), ResumeState::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
