//! Messages between the engine and its owner (the UI, or `lumen-play`).

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use lumen_core::{RepeatMode, ReplayGainMode, TrackId};

/// One entry of the play queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueItem {
    /// Library id, when the track comes from the library.
    pub id: Option<TrackId>,
    pub path: PathBuf,
}

impl QueueItem {
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self {
            id: None,
            path: path.into(),
        }
    }
}

/// Which device and sharing mode to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    /// WASAPI endpoint id; `None` = system default device.
    pub device_id: Option<String>,
    /// Try WASAPI exclusive mode first (bit-perfect).
    pub exclusive: bool,
    /// Exclusive mode: open the device at this rate and resample every file
    /// to it (an explicit choice, shown in the badge). `None` = each file's
    /// own rate (bit-perfect). Shared mode always uses the Windows mix rate.
    pub rate: Option<u32>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            device_id: None,
            exclusive: true,
            rate: None,
        }
    }
}

/// Commands sent to the engine. None of them block the sender.
#[derive(Clone, Debug)]
pub enum Command {
    /// Replace the queue. `start` is an item index; playback starts at
    /// `position`. With `play: false` the engine loads the track paused without
    /// opening the device (used to resume the last session on launch).
    SetQueue {
        items: Vec<QueueItem>,
        start: usize,
        position: Duration,
        play: bool,
    },
    Play,
    Pause,
    TogglePause,
    /// Stop and close the device. The queue is kept.
    Stop,
    /// Seek within the current track.
    Seek(Duration),
    Next,
    /// Restarts the track if more than 3 s have played, else goes back.
    Prev,
    /// Play this queue item index from the start.
    PlayIndex(usize),
    /// Add items to the queue without interrupting playback: right after the
    /// current track (`next: true`, "Play next") or at the end ("Add to
    /// queue"). New items get indices after the existing ones. With nothing
    /// queued, the items become the queue, loaded paused.
    Enqueue {
        items: Vec<QueueItem>,
        next: bool,
    },
    /// Linear gain 0.0..=1.0 applied digitally just before output.
    SetVolume(f32),
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    SetOutput(OutputConfig),
    SetReplayGain(ReplayGainMode),
    SetGapless(bool),
    /// Answered with [`Event::Devices`].
    ListDevices,
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

/// Source format and tags of a decoded track.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackInfo {
    /// Codec short name from the decoder ("flac", "mp3", "alac", "vorbis", "pcm_s24le", …).
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Bits per sample for lossless sources; `None` for lossy codecs.
    pub bits_per_sample: Option<u32>,
    pub duration: Option<Duration>,
    pub replaygain: ReplayGainTags,
}

impl TrackInfo {
    pub fn is_lossless(&self) -> bool {
        self.bits_per_sample.is_some()
    }

    /// Display name of the codec ("FLAC", "ALAC", "MP3", "Vorbis", "WAV").
    pub fn codec_label(&self) -> String {
        let c = self.codec.as_str();
        match c {
            "flac" | "alac" | "mp3" | "mp2" | "mp1" | "aac" => c.to_ascii_uppercase(),
            "vorbis" => "Vorbis".into(),
            _ if c.starts_with("pcm") => "WAV".into(),
            _ => c.to_ascii_uppercase(),
        }
    }

    /// Now Playing format badge, e.g. "FLAC 24-bit / 96 kHz" or "MP3 / 44.1 kHz".
    pub fn format_badge(&self) -> String {
        match self.bits_per_sample {
            Some(bits) => format!(
                "{} {bits}-bit / {}",
                self.codec_label(),
                format_khz(self.sample_rate)
            ),
            None => format!("{} / {}", self.codec_label(), format_khz(self.sample_rate)),
        }
    }
}

/// ReplayGain values read from tags.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReplayGainTags {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// Sample layout written to the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// Signed integer PCM: `container` bits per sample of which the top
    /// `valid` bits carry audio (e.g. 24-in-32).
    Int {
        container: u16,
        valid: u16,
    },
    Float32,
}

impl SampleFormat {
    pub fn container_bytes(self) -> usize {
        match self {
            SampleFormat::Int { container, .. } => usize::from(container / 8),
            SampleFormat::Float32 => 4,
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            SampleFormat::Int { container, valid } if container == valid => {
                write!(f, "{valid}-bit int")
            }
            SampleFormat::Int { container, valid } => write!(f, "{valid}-bit int in {container}"),
            SampleFormat::Float32 => write!(f, "32-bit float"),
        }
    }
}

/// Format of an open device stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamFormat {
    pub rate: u32,
    pub channels: u16,
    pub sample: SampleFormat,
}

impl fmt::Display for StreamFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz, {} ch, {}", self.rate, self.channels, self.sample)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Exclusive,
    Shared,
}

/// Everything between the file and the speaker, for the Now Playing badges.
#[derive(Clone, Debug, PartialEq)]
pub struct SignalPath {
    pub source: TrackInfo,
    pub device_name: String,
    pub mode: OutputMode,
    pub output: StreamFormat,
    /// `Some((from, to))` when rubato resamples.
    pub resampled: Option<(u32, u32)>,
    /// Why exclusive mode was not used although it was requested.
    pub exclusive_fallback: Option<String>,
    /// ReplayGain applied to this track, in dB.
    pub replaygain_db: Option<f32>,
    /// Digital volume below 100% changes samples.
    pub volume_attenuates: bool,
    /// True when the samples reach the device unchanged: exclusive mode, no
    /// resampling, container wide enough, no ReplayGain and volume at 100%.
    pub bit_perfect: bool,
}

impl SignalPath {
    /// Badge text for the output mode, as the spec words it.
    pub fn mode_badge(&self) -> String {
        match (self.mode, self.resampled) {
            (OutputMode::Exclusive, None) => "WASAPI Exclusive".to_string(),
            (OutputMode::Exclusive, Some((_, to))) => {
                format!("WASAPI Exclusive · resampled to {}", format_khz(to))
            }
            (OutputMode::Shared, Some((_, to))) => {
                format!("Shared · resampled to {}", format_khz(to))
            }
            (OutputMode::Shared, None) => "Shared".to_string(),
        }
    }
}

/// `48000` → "48 kHz", `44100` → "44.1 kHz".
pub fn format_khz(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        let khz = format!("{:.1}", rate as f64 / 1000.0);
        format!("{} kHz", khz.trim_end_matches(".0"))
    }
}

/// A WASAPI render endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Events from the engine, in order.
#[derive(Clone, Debug)]
pub enum Event {
    StateChanged(PlaybackState),
    /// The audible track changed (also sent after `SetQueue`, `Next`, …).
    TrackChanged {
        index: usize,
        item: QueueItem,
        info: TrackInfo,
    },
    /// About 30 Hz while playing; once after seeks and state changes.
    Position {
        index: usize,
        position: Duration,
        duration: Option<Duration>,
    },
    SignalPath(SignalPath),
    /// Queue order or modes changed. `order` lists item indices in play order.
    QueueChanged {
        order: Vec<usize>,
        current: Option<usize>,
        shuffle: bool,
        repeat: RepeatMode,
    },
    /// A queue item could not be played and was skipped.
    TrackSkipped {
        index: usize,
        path: PathBuf,
        reason: String,
    },
    /// Playback reached the end of the queue.
    EndOfQueue,
    Devices(Vec<DeviceInfo>),
    /// The device ran out of samples while playing (total count so far).
    Underruns(u64),
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn khz_formatting() {
        assert_eq!(format_khz(48_000), "48 kHz");
        assert_eq!(format_khz(44_100), "44.1 kHz");
        assert_eq!(format_khz(88_200), "88.2 kHz");
        assert_eq!(format_khz(192_000), "192 kHz");
    }

    #[test]
    fn format_badges() {
        let mut info = TrackInfo {
            codec: "flac".into(),
            sample_rate: 96_000,
            channels: 2,
            bits_per_sample: Some(24),
            duration: None,
            replaygain: ReplayGainTags::default(),
        };
        assert_eq!(info.format_badge(), "FLAC 24-bit / 96 kHz");
        info.codec = "mp3".into();
        info.bits_per_sample = None;
        info.sample_rate = 44_100;
        assert_eq!(info.format_badge(), "MP3 / 44.1 kHz");
        info.codec = "pcm_s16le".into();
        info.bits_per_sample = Some(16);
        assert_eq!(info.format_badge(), "WAV 16-bit / 44.1 kHz");
    }

    #[test]
    fn sample_format_display() {
        let f = SampleFormat::Int {
            container: 32,
            valid: 24,
        };
        assert_eq!(f.to_string(), "24-bit int in 32");
        assert_eq!(f.container_bytes(), 4);
        assert_eq!(SampleFormat::Float32.to_string(), "32-bit float");
    }
}
