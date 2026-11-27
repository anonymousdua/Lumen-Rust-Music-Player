//! Lumen lyrics: LRC parsing (standard and enhanced word timings, the
//! `[offset:]` tag), embedded lyrics (ID3 `SYLT`/`USLT`, Vorbis `LYRICS`),
//! and a time index that answers "which line, and how much of it, is sung at
//! time t".
//!
//! ```
//! let lyrics = lumen_lyrics::parse_lrc("[00:01.00]Hello\n[00:03.50]World");
//! assert!(lyrics.synced);
//! assert_eq!(lyrics.line_at(2_000), Some(0));
//! assert_eq!(lyrics.line_at(4_000), Some(1));
//! ```

mod embedded;
mod lrc;
mod text;

use std::path::Path;

pub use embedded::{parse_sylt, read_embedded};
pub use lrc::parse_lrc;
pub use text::decode_text;

/// One timed word (enhanced LRC or word-level SYLT).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// Start time, ms from the start of the track.
    pub start_ms: u64,
    /// Text including any trailing space.
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Start time; `None` in plain (unsynced) lyrics.
    pub time_ms: Option<u64>,
    pub text: String,
    /// Word timings, empty unless the source has them.
    pub words: Vec<Word>,
    /// When the last word ends, if the source says (enhanced LRC's trailing
    /// `<mm:ss.xx>`); otherwise the next line's start is used.
    pub end_ms: Option<u64>,
}

/// Where lyrics came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    LrcFile,
    /// ID3 `SYLT`.
    EmbeddedSynced,
    /// ID3 `USLT`, Vorbis/MP4 `LYRICS` (may still contain LRC timestamps).
    EmbeddedText,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    /// Lines have timestamps (sorted by time).
    pub synced: bool,
    pub source: Option<Source>,
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.text.trim().is_empty())
    }

    /// Any line has word timings.
    pub fn has_words(&self) -> bool {
        self.synced && self.lines.iter().any(|l| l.words.len() > 1)
    }

    /// Index of the line sung at `t_ms`: the last line starting at or
    /// before `t_ms`. `None` before the first line or for plain lyrics.
    pub fn line_at(&self, t_ms: i64) -> Option<usize> {
        if !self.synced || t_ms < 0 {
            return None;
        }
        let t = t_ms as u64;
        let n = self
            .lines
            .partition_point(|l| l.time_ms.is_some_and(|s| s <= t));
        n.checked_sub(1)
    }

    /// End of line `i`: its own end tag, else the next line's start.
    pub fn line_end(&self, i: usize) -> Option<u64> {
        let line = self.lines.get(i)?;
        line.end_ms
            .or_else(|| self.lines.get(i + 1).and_then(|l| l.time_ms))
    }

    /// How far word highlighting has got in line `i` at `t_ms`: the number of
    /// fully sung words and how much (0..1) of the next word is sung.
    pub fn word_progress(&self, i: usize, t_ms: i64) -> (usize, f32) {
        let Some(line) = self.lines.get(i) else {
            return (0, 0.0);
        };
        let t = t_ms.max(0) as u64;
        let words = &line.words;
        for (k, w) in words.iter().enumerate() {
            if t < w.start_ms {
                return (k, 0.0);
            }
            let end = words
                .get(k + 1)
                .map(|n| n.start_ms)
                .or_else(|| self.line_end(i))
                .unwrap_or(w.start_ms + 1_000);
            if t < end {
                let span = end.saturating_sub(w.start_ms).max(1) as f32;
                return (k, (t - w.start_ms) as f32 / span);
            }
        }
        (words.len(), 0.0)
    }
}

/// Where to look for lyrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadOptions {
    /// `song.lrc` next to `song.flac`.
    pub lrc_file: bool,
    /// Tags inside the audio file.
    pub embedded: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            lrc_file: true,
            embedded: true,
        }
    }
}

/// Loads the best lyrics for `audio`: synced beats plain; among equals the
/// `.lrc` file beats the tags.
pub fn load(audio: &Path, opts: LoadOptions) -> Option<Lyrics> {
    let from_file = opts
        .lrc_file
        .then(|| {
            let bytes = std::fs::read(audio.with_extension("lrc")).ok()?;
            let mut l = parse_lrc(&decode_text(&bytes));
            l.source = Some(Source::LrcFile);
            Some(l)
        })
        .flatten()
        .filter(|l| !l.is_empty());
    if from_file.as_ref().is_some_and(|l| l.synced) {
        return from_file;
    }
    let embedded = opts
        .embedded
        .then(|| read_embedded(audio))
        .flatten()
        .filter(|l| !l.is_empty());
    match (from_file, embedded) {
        (_, Some(e)) if e.synced => Some(e),
        (Some(f), _) => Some(f),
        (None, e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_finds_lines_and_words() {
        let l = parse_lrc(
            "[00:01.00]<00:01.00>One <00:01.50>two <00:02.00>three<00:03.00>\n\
             [00:04.00]Four\n[00:06.00]",
        );
        assert_eq!(l.line_at(500), None);
        assert_eq!(l.line_at(1_000), Some(0));
        assert_eq!(l.line_at(3_999), Some(0));
        assert_eq!(l.line_at(4_000), Some(1));
        assert_eq!(l.line_at(60_000), Some(2));
        assert!(l.has_words());
        assert_eq!(l.word_progress(0, 900), (0, 0.0));
        assert_eq!(l.word_progress(0, 1_250), (0, 0.5));
        assert_eq!(l.word_progress(0, 1_500), (1, 0.0));
        assert_eq!(l.word_progress(0, 2_500), (2, 0.5));
        assert_eq!(l.word_progress(0, 3_100), (3, 0.0));
        assert_eq!(l.line_end(1), Some(6_000));
    }

    #[test]
    fn plain_lyrics_have_no_active_line() {
        let l = parse_lrc("[ar:Someone]\nFirst line\nSecond line\n");
        assert!(!l.synced);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.line_at(10_000), None);
    }

    #[test]
    fn load_prefers_synced_lrc_then_embedded() {
        let dir = std::env::temp_dir().join(format!("lumen-lyrics-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let audio = dir.join("song.flac");
        std::fs::write(&audio, b"not audio").expect("w");
        assert_eq!(load(&audio, LoadOptions::default()), None);
        std::fs::write(dir.join("song.lrc"), "[00:01.00]चाँद\n").expect("w");
        let l = load(&audio, LoadOptions::default()).expect("lyrics");
        assert_eq!(l.source, Some(Source::LrcFile));
        assert_eq!(l.lines[0].text, "चाँद");
        assert_eq!(
            load(
                &audio,
                LoadOptions {
                    lrc_file: false,
                    embedded: true
                }
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
