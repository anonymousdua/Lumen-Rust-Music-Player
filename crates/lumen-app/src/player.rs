//! The UI's handle on the audio engine: the queue as the UI knows it, the
//! current track, play state and position, shuffle and repeat, play
//! counting, resume-on-launch and the Windows media controls.
//!
//! `Player` is a GPUI entity. Position ticks only notify the entity itself
//! (the player bars observe it); track, state and queue changes are also
//! emitted as [`PlayerEvent`]s so the library screens redraw only when what
//! they show changes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{Context, EventEmitter, Subscription};
use lumen_audio::{
    Command, DeviceInfo, Engine, EngineConfig, Event, OutputConfig, OutputMode, PlaybackState,
    QueueItem, SignalPath, TrackInfo,
};
use lumen_core::{
    AudioSettings, LyricsSettings, RepeatMode, ReplayGainMode, ResumeState, Track, TrackId,
};
use lumen_library::Library;
use lumen_lyrics::{LoadOptions, Lyrics};

use crate::smtc::{MediaButton, MediaInfo, Smtc};

/// Coarse changes the library screens care about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The audible track, play state, queue or output changed.
    Changed,
    /// An engine error worth showing.
    Error,
    /// A position update (about 30 Hz while playing). Only the lyrics view
    /// listens, so word highlighting can run smoothly without redrawing the
    /// rest of the window.
    Tick,
}

/// A play is counted after half the track or 4 minutes, whichever is first.
const PLAY_COUNT_CAP: Duration = Duration::from_secs(240);
/// How often the resume file is refreshed while playing.
const SAVE_EVERY: Duration = Duration::from_secs(10);
/// ← / → seek step and Ctrl+↑ / Ctrl+↓ volume step.
pub const SEEK_STEP: Duration = Duration::from_secs(5);
pub const VOLUME_STEP: f32 = 0.05;

pub struct Player {
    engine: Option<Engine>,
    library: Option<Arc<Library>>,
    /// Mirrors the engine's queue items (same indices).
    items: Vec<Track>,
    /// Queue item index of the audible track.
    pub current: Option<usize>,
    /// Play order (item indices) from the engine.
    order: Vec<usize>,
    pub info: Option<TrackInfo>,
    pub signal: Option<SignalPath>,
    pub state: PlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Last engine error or skipped file, for display.
    pub message: Option<String>,
    /// Time actually listened to the current track (seeks don't count).
    listened: Duration,
    last_position: Duration,
    counted: bool,
    /// Last position bucket that caused a redraw (4 Hz).
    redraw_bucket: u64,
    resume_file: PathBuf,
    last_save: Instant,
    thumbs: PathBuf,
    smtc: Option<Smtc>,
    _quit: Subscription,
    /// Lyrics of the audible track (`None` while loading or when none).
    pub lyrics: Option<Arc<Lyrics>>,
    /// Which track `lyrics` belongs to (also set while loading).
    lyrics_for: Option<TrackId>,
    /// Per-track offset from the database; positive shows lyrics later.
    pub lyrics_offset_ms: i32,
    pub lyrics_prefs: LyricsSettings,
    lyrics_loading: bool,
    /// WASAPI render devices (Settings › Audio), refreshed on request.
    pub devices: Vec<DeviceInfo>,
    /// Device underruns reported by the engine (dropouts).
    pub underruns: u64,
}

impl EventEmitter<PlayerEvent> for Player {}

impl Player {
    pub fn new(
        library: Option<Arc<Library>>,
        data_dir: PathBuf,
        thumbs: PathBuf,
        audio: &AudioSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = EngineConfig {
            output: output_config(audio),
            replaygain: replaygain_mode(audio),
            gapless: audio.gapless,
            ..EngineConfig::default()
        };
        let engine = match Engine::start(config) {
            Ok(e) => {
                Self::bridge_events(e.events().clone(), cx);
                Some(e)
            }
            Err(e) => {
                tracing::error!("audio engine failed to start: {e}");
                None
            }
        };
        let message = engine
            .is_none()
            .then(|| "The audio engine could not start.".to_string());
        let quit = cx.on_app_quit(|this: &mut Self, _| {
            this.save_resume();
            async {}
        });
        Self {
            engine,
            library,
            items: Vec::new(),
            current: None,
            order: Vec::new(),
            info: None,
            signal: None,
            state: PlaybackState::Stopped,
            position: Duration::ZERO,
            duration: None,
            volume: 1.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            message,
            listened: Duration::ZERO,
            last_position: Duration::ZERO,
            counted: false,
            redraw_bucket: u64::MAX,
            resume_file: ResumeState::file_in(&data_dir),
            last_save: Instant::now(),
            thumbs,
            smtc: None,
            _quit: quit,
            lyrics: None,
            lyrics_for: None,
            lyrics_offset_ms: 0,
            lyrics_prefs: LyricsSettings::default(),
            lyrics_loading: false,
            devices: Vec::new(),
            underruns: 0,
        }
    }

    fn bridge_events(events: crossbeam_channel::Receiver<Event>, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Event>();
        let spawned = std::thread::Builder::new()
            .name("lumen-audio-events".into())
            .spawn(move || {
                for e in events.iter() {
                    if tx.unbounded_send(e).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!("cannot forward audio events: {e}");
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while let Ok(e) = rx.try_recv() {
                    batch.push(e);
                }
                if this.update(cx, |p, cx| p.apply(batch, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Connects the Windows media controls (media keys, the volume overlay)
    /// to the main window.
    pub fn attach_smtc(&mut self, hwnd: isize, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<MediaButton>();
        match Smtc::new(hwnd, move |b| {
            let _ = tx.unbounded_send(b);
        }) {
            Ok(s) => self.smtc = Some(s),
            Err(e) => {
                tracing::warn!("Windows media controls unavailable: {e}");
                return;
            }
        }
        cx.spawn(async move |this, cx| {
            while let Some(b) = rx.next().await {
                let alive = this.update(cx, |p, cx| match b {
                    MediaButton::Play => p.send(Command::Play),
                    MediaButton::Pause => p.send(Command::Pause),
                    MediaButton::Stop => p.send(Command::Pause),
                    MediaButton::Next => p.next(),
                    MediaButton::Previous => p.prev(),
                    MediaButton::Seek(to) => p.seek_to(to, cx),
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        self.sync_smtc(true);
    }

    fn send(&self, c: Command) {
        if let Some(e) = &self.engine {
            e.send(c);
        }
    }

    /// The audible track.
    pub fn track(&self) -> Option<&Track> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn item(&self, index: usize) -> Option<&Track> {
        self.items.get(index)
    }

    pub fn current_id(&self) -> Option<TrackId> {
        self.track().map(|t| t.id)
    }

    pub fn is_playing(&self) -> bool {
        self.state == PlaybackState::Playing
    }

    /// Length of the audible track (decoder value, else the tag value).
    pub fn total(&self) -> Duration {
        self.duration
            .or_else(|| self.info.as_ref().and_then(|i| i.duration))
            .or_else(|| self.track().map(|t| Duration::from_millis(t.duration_ms)))
            .unwrap_or_default()
    }

    pub fn exclusive(&self) -> bool {
        self.signal
            .as_ref()
            .is_some_and(|s| s.mode == OutputMode::Exclusive)
    }

    /// Up to `n` queue item indices that play after the current one.
    pub fn upcoming(&self, n: usize) -> Vec<usize> {
        let Some(cur) = self.current else {
            return Vec::new();
        };
        let Some(pos) = self.order.iter().position(|&i| i == cur) else {
            return Vec::new();
        };
        let mut out: Vec<usize> = self.order[pos + 1..].iter().take(n).copied().collect();
        if out.len() < n && self.repeat != RepeatMode::Off && !self.shuffle {
            out.extend(
                self.order
                    .iter()
                    .copied()
                    .filter(|&i| i != cur)
                    .take(n - out.len()),
            );
        }
        out
    }

    /// Replaces the queue with `tracks` and plays `start`.
    pub fn play_list(&mut self, tracks: Vec<Track>, start: usize, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        let items = tracks.iter().map(queue_item).collect();
        self.items = tracks;
        self.message = None;
        self.send(Command::SetQueue {
            items,
            start,
            position: Duration::ZERO,
            play: true,
        });
        cx.notify();
    }

    /// Loads the last session's queue, paused at its position (launch).
    pub fn restore(
        &mut self,
        tracks: Vec<Track>,
        start: usize,
        position: Duration,
        state: &ResumeState,
        cx: &mut Context<Self>,
    ) {
        self.set_volume(state.volume, cx);
        self.shuffle = state.shuffle;
        self.repeat = state.repeat;
        self.send(Command::SetShuffle(state.shuffle));
        self.send(Command::SetRepeat(state.repeat));
        if tracks.is_empty() {
            return;
        }
        let items = tracks.iter().map(queue_item).collect();
        self.items = tracks;
        self.send(Command::SetQueue {
            items,
            start,
            position,
            play: false,
        });
        cx.notify();
    }

    /// "Play next" (`next`) or "Add to queue".
    pub fn enqueue(&mut self, tracks: Vec<Track>, next: bool, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        let items = tracks.iter().map(queue_item).collect();
        if self.current.is_none() {
            // The engine makes these the whole queue.
            self.items.clear();
        }
        self.items.extend(tracks);
        self.send(Command::Enqueue { items, next });
        cx.notify();
    }

    pub fn set_shuffle(&mut self, on: bool, cx: &mut Context<Self>) {
        self.shuffle = on;
        self.send(Command::SetShuffle(on));
        cx.notify();
    }

    pub fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        self.set_shuffle(!self.shuffle, cx);
        self.save_resume();
    }

    pub fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.repeat = self.repeat.cycle();
        self.send(Command::SetRepeat(self.repeat));
        self.save_resume();
        cx.notify();
    }

    pub fn toggle(&mut self) {
        self.send(Command::TogglePause);
    }

    pub fn next(&mut self) {
        self.send(Command::Next);
    }

    pub fn prev(&mut self) {
        self.send(Command::Prev);
    }

    /// Plays queue item `index` (Up Next).
    pub fn play_index(&mut self, index: usize) {
        self.send(Command::PlayIndex(index));
    }

    pub fn seek_to(&mut self, to: Duration, cx: &mut Context<Self>) {
        if self.track().is_none() {
            return;
        }
        let total = self.total();
        let to = if total.is_zero() {
            to
        } else {
            to.min(total.saturating_sub(Duration::from_millis(250)))
        };
        self.position = to;
        self.last_position = to;
        self.send(Command::Seek(to));
        self.sync_smtc(false);
        cx.emit(PlayerEvent::Tick);
        cx.notify();
    }

    pub fn seek_fraction(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let to = self.total().mul_f32(fraction.clamp(0.0, 1.0));
        self.seek_to(to, cx);
    }

    /// ← / →: seek by `SEEK_STEP` back (`forward: false`) or forward.
    pub fn seek_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let to = if forward {
            self.position + SEEK_STEP
        } else {
            self.position.saturating_sub(SEEK_STEP)
        };
        self.seek_to(to, cx);
    }

    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume.clamp(0.0, 1.0);
        self.send(Command::SetVolume(self.volume));
        cx.notify();
    }

    pub fn volume_step(&mut self, up: bool, cx: &mut Context<Self>) {
        let v = if up {
            self.volume + VOLUME_STEP
        } else {
            self.volume - VOLUME_STEP
        };
        // Snap to the 5% grid so repeated steps land on 100% exactly.
        self.set_volume((v / VOLUME_STEP).round() * VOLUME_STEP, cx);
        self.save_resume();
    }

    /// Applies Settings › Audio. Changing the device or mode reopens the
    /// output at the current position.
    pub fn apply_audio(&mut self, audio: &AudioSettings) {
        self.send(Command::SetOutput(output_config(audio)));
        self.send(Command::SetGapless(audio.gapless));
        self.send(Command::SetReplayGain(replaygain_mode(audio)));
    }

    /// Asks the engine for the device list (answered with `Devices`).
    pub fn refresh_devices(&self) {
        self.send(Command::ListDevices);
    }

    /// Lyrics for the audible track are still being read.
    pub fn lyrics_loading(&self) -> bool {
        self.lyrics_loading
    }

    /// Lyrics clock: the position shifted by the per-track offset.
    pub fn lyrics_time_ms(&self) -> i64 {
        self.position.as_millis() as i64 - i64::from(self.lyrics_offset_ms)
    }

    /// Seeks so that the lyrics clock reads `lyrics_ms` (click on a line).
    pub fn seek_to_lyrics(&mut self, lyrics_ms: u64, cx: &mut Context<Self>) {
        let ms = (lyrics_ms as i64 + i64::from(self.lyrics_offset_ms)).max(0) as u64;
        self.seek_to(Duration::from_millis(ms), cx);
    }

    /// The ±0.5 s buttons: shifts this track's lyrics and saves it.
    pub fn nudge_lyrics_offset(&mut self, delta_ms: i32, cx: &mut Context<Self>) {
        self.lyrics_offset_ms = (self.lyrics_offset_ms + delta_ms).clamp(-60_000, 60_000);
        if let (Some(lib), Some(id)) = (&self.library, self.current_id()) {
            lib.set_lyrics_offset(id, self.lyrics_offset_ms);
        }
        cx.emit(PlayerEvent::Changed);
        cx.notify();
    }

    /// Offset as stored in the library (after a reload or a menu edit).
    pub fn sync_lyrics_offset(&mut self, ms: i32, cx: &mut Context<Self>) {
        if self.lyrics_offset_ms != ms {
            self.lyrics_offset_ms = ms;
            cx.emit(PlayerEvent::Changed);
            cx.notify();
        }
    }

    /// New lyrics settings; reloads the lyrics if the sources changed.
    pub fn set_lyrics_prefs(&mut self, prefs: LyricsSettings, cx: &mut Context<Self>) {
        let reload = prefs.load_lrc != self.lyrics_prefs.load_lrc
            || prefs.embedded != self.lyrics_prefs.embedded;
        self.lyrics_prefs = prefs;
        if reload {
            self.lyrics_for = None;
            self.load_lyrics(cx);
        }
        cx.emit(PlayerEvent::Changed);
        cx.notify();
    }

    /// Loads lyrics for the audible track on a background thread.
    fn load_lyrics(&mut self, cx: &mut Context<Self>) {
        let Some(track) = self.track() else {
            self.lyrics = None;
            self.lyrics_for = None;
            return;
        };
        if self.lyrics_for == Some(track.id) {
            return;
        }
        let id = track.id;
        let path = track.path.clone();
        self.lyrics_offset_ms = track.lyrics_offset_ms;
        self.lyrics_for = Some(id);
        self.lyrics = None;
        self.lyrics_loading = true;
        let opts = LoadOptions {
            lrc_file: self.lyrics_prefs.load_lrc,
            embedded: self.lyrics_prefs.embedded,
        };
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { lumen_lyrics::load(&path, opts) })
                .await;
            let _ = this.update(cx, |p, cx| {
                if p.lyrics_for == Some(id) {
                    p.lyrics = loaded.map(Arc::new);
                    p.lyrics_loading = false;
                    cx.emit(PlayerEvent::Changed);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn apply(&mut self, events: Vec<Event>, cx: &mut Context<Self>) {
        let mut tick = false;
        let mut redraw = false;
        let mut changed = false;
        let mut track_changed = false;
        let mut error = false;
        for e in events {
            match e {
                Event::StateChanged(s) => {
                    self.state = s;
                    changed = true;
                }
                Event::TrackChanged { index, info, .. } => {
                    // Also on repeat-one: each pass is a new play.
                    self.listened = Duration::ZERO;
                    self.counted = false;
                    self.current = Some(index);
                    self.duration = info.duration;
                    self.info = Some(info);
                    self.position = Duration::ZERO;
                    self.last_position = Duration::ZERO;
                    changed = true;
                    track_changed = true;
                }
                Event::Position {
                    index,
                    position,
                    duration,
                } => {
                    if Some(index) == self.current {
                        self.count_listening(position);
                    }
                    tick = true;
                    self.position = position;
                    if duration.is_some() {
                        self.duration = duration;
                    }
                    let bucket = position.as_millis() as u64 / 250;
                    if bucket != self.redraw_bucket {
                        self.redraw_bucket = bucket;
                        redraw = true;
                    }
                }
                Event::SignalPath(s) => {
                    self.signal = Some(s);
                    changed = true;
                }
                Event::QueueChanged {
                    order,
                    current,
                    shuffle,
                    repeat,
                } => {
                    self.order = order;
                    if current.is_some() {
                        self.current = current;
                    }
                    self.shuffle = shuffle;
                    self.repeat = repeat;
                    changed = true;
                }
                Event::TrackSkipped { path, reason, .. } => {
                    tracing::warn!("skipped {}: {reason}", path.display());
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.message = Some(format!("Skipped {name}: {reason}"));
                    error = true;
                }
                Event::Error(msg) => {
                    tracing::warn!("audio: {msg}");
                    self.message = Some(msg);
                    error = true;
                }
                Event::EndOfQueue => {
                    self.position = Duration::ZERO;
                    changed = true;
                }
                Event::Devices(d) => {
                    self.devices = d;
                    changed = true;
                }
                Event::Underruns(n) => {
                    // The device ran dry: an audible dropout. Logged so
                    // performance problems can be traced.
                    tracing::warn!("audio dropout: {n} underruns so far");
                    self.underruns = n;
                }
            }
        }
        if track_changed {
            self.load_lyrics(cx);
        }
        if tick {
            cx.emit(PlayerEvent::Tick);
        }
        if changed {
            self.sync_smtc(track_changed);
            self.save_resume();
            cx.emit(PlayerEvent::Changed);
        } else if self.is_playing() && self.last_save.elapsed() >= SAVE_EVERY {
            self.save_resume();
        }
        if error {
            cx.emit(PlayerEvent::Error);
        }
        if redraw || changed || error {
            cx.notify();
        }
    }

    /// Counts a play once enough of the track has actually been heard.
    fn count_listening(&mut self, position: Duration) {
        if self.state == PlaybackState::Playing
            && position > self.last_position
            && position - self.last_position < Duration::from_secs(2)
        {
            self.listened += position - self.last_position;
        }
        self.last_position = position;
        if self.counted {
            return;
        }
        let needed = play_count_threshold(self.total());
        if self.listened >= needed
            && !needed.is_zero()
            && let (Some(lib), Some(id)) = (&self.library, self.current_id())
        {
            self.counted = true;
            lib.record_play(id);
        }
    }

    /// Saves the queue, track, position, volume, shuffle and repeat.
    pub fn save_resume(&mut self) {
        self.last_save = Instant::now();
        let state = ResumeState {
            queue: self.items.iter().map(|t| t.id.0).collect(),
            current: self.current.unwrap_or(0),
            position_ms: if self.state == PlaybackState::Stopped {
                0
            } else {
                self.position.as_millis() as u64
            },
            volume: (self.volume * 1000.0).round() / 1000.0,
            shuffle: self.shuffle,
            repeat: self.repeat,
        };
        if let Err(e) = state.save(&self.resume_file) {
            tracing::warn!("cannot save playback state: {e}");
        }
    }

    /// Pushes the current track, play state and timeline to Windows.
    fn sync_smtc(&self, track_changed: bool) {
        let Some(smtc) = &self.smtc else { return };
        if track_changed || self.track().is_none() {
            smtc.set_info(self.track().map(|t| {
                MediaInfo {
                    title: t.title.clone(),
                    artist: t.artist.clone(),
                    album: t.album.clone(),
                    thumbnail: t
                        .cover_hash
                        .as_ref()
                        .map(|h| self.thumbs.join(format!("{h}.jpg")))
                        .filter(|p| p.is_file()),
                }
            }));
        }
        smtc.set_state(self.state, self.track().is_some());
        smtc.set_timeline(self.position, self.total());
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
    }
}

/// Half the track, but never more than 4 minutes.
pub fn play_count_threshold(total: Duration) -> Duration {
    (total / 2).min(PLAY_COUNT_CAP)
}

fn output_config(a: &AudioSettings) -> OutputConfig {
    OutputConfig {
        device_id: a.device_id.clone(),
        exclusive: a.exclusive,
        rate: a.sample_rate,
    }
}

/// The ReplayGain toggle uses track gain: it suits shuffled and mixed queues.
fn replaygain_mode(a: &AudioSettings) -> ReplayGainMode {
    if a.replaygain {
        ReplayGainMode::Track
    } else {
        ReplayGainMode::Off
    }
}

fn queue_item(t: &Track) -> QueueItem {
    QueueItem {
        id: Some(t.id),
        path: t.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_count_at_half_or_four_minutes() {
        assert_eq!(
            play_count_threshold(Duration::from_secs(180)),
            Duration::from_secs(90)
        );
        assert_eq!(
            play_count_threshold(Duration::from_secs(600)),
            Duration::from_secs(240)
        );
        assert_eq!(
            play_count_threshold(Duration::from_secs(480)),
            Duration::from_secs(240)
        );
    }
}
