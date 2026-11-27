//! The playback engine: a control/decoder thread that owns the queue, decodes
//! ahead into the output ring buffer and reports events.
//!
//! Threads:
//! - **control** (above-normal priority): commands in, events out, decoding,
//!   ReplayGain, channel mapping and resampling. Never touches the UI or DB.
//! - **output** (MMCSS Pro Audio, one per open stream): pulls from the ring
//!   buffer and feeds the device. See [`crate::output`].
//!
//! Positions are tracked in "segments": every track that enters the ring
//! buffer records the frame index where it starts, so the engine knows which
//! track is audible even while the next one is already being decoded
//! (gapless playback).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use lumen_core::{RepeatMode, ReplayGainMode};
use rtrb::Producer;

use crate::decode::Decoder;
use crate::dsp::{ChannelMap, replaygain_factor};
use crate::negotiate::{SourceFormat, carries_exactly, exclusive_stream_fits};
use crate::output::{Backend, OpenRequest, OutputStream, panic_message};
use crate::protocol::{
    Command, Event, OutputConfig, OutputMode, PlaybackState, QueueItem, SignalPath, StreamFormat,
    TrackInfo,
};
use crate::queue::{PrevAction, Queue, Rng};
use crate::resample::Resampler;

/// Position events while playing (~30 Hz).
const POSITION_INTERVAL: Duration = Duration::from_millis(33);
/// How often the control thread wakes while playing to refill the ring.
const PUMP_INTERVAL: Duration = Duration::from_millis(10);

/// Engine start-up settings.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub backend: Backend,
    pub output: OutputConfig,
    pub replaygain: ReplayGainMode,
    pub gapless: bool,
    /// Linear 0.0..=1.0.
    pub volume: f32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            backend: Backend::Wasapi,
            output: OutputConfig::default(),
            replaygain: ReplayGainMode::Off,
            gapless: true,
            volume: 1.0,
        }
    }
}

/// Handle to a running engine. Dropping it shuts the engine down.
pub struct Engine {
    commands: Sender<Command>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start(config: EngineConfig) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("lumen-engine".into())
            .spawn(move || {
                crate::priority::raise_current_thread();
                #[cfg(windows)]
                let _ = wasapi::initialize_mta();
                let panic_tx = ev_tx.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    Control::new(config, ev_tx).run(cmd_rx);
                }));
                if let Err(p) = result {
                    let msg = panic_message(&p);
                    tracing::error!("engine thread panicked: {msg}");
                    let _ = panic_tx.send(Event::Error(format!("audio engine crashed: {msg}")));
                }
            })
            .map_err(|e| format!("cannot start the audio engine: {e}"))?;
        Ok(Self {
            commands: cmd_tx,
            events: ev_rx,
            thread: Some(thread),
        })
    }

    /// Sends a command. Never blocks.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// A sender other threads can keep.
    pub fn sender(&self) -> Sender<Command> {
        self.commands.clone()
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A track being decoded.
struct Active {
    index: usize,
    decoder: Decoder,
    /// ReplayGain `(factor, dB)`.
    gain: Option<(f64, f32)>,
    start_offset: Duration,
}

/// Outcome of one decode step in [`Control::fill`].
enum Step {
    Continue,
    Error(String),
    TrackDone,
    TrackFailed(String),
}

/// Where a track begins in the stream.
struct Segment {
    /// Ring-buffer frame index (same counter as `StreamShared::played`).
    start: u64,
    index: usize,
    offset: Duration,
    rate: u32,
    info: TrackInfo,
    signal: SignalPath,
    /// Queue state to adopt when this segment becomes audible.
    queue: Option<Queue>,
    announced: bool,
}

/// An open output stream plus the processing chain feeding it.
struct Out {
    stream: OutputStream,
    producer: Producer<f64>,
    format: StreamFormat,
    /// Shared fallback stream: the source rate exclusive mode refused.
    fallback_rate: Option<u32>,
    pushed: u64,
    resampler: Option<Resampler>,
    map: ChannelMap,
    pending: Vec<f64>,
    pending_pos: usize,
    scratch: Vec<f64>,
    mapped: Vec<f64>,
}

impl Out {
    fn channels(&self) -> usize {
        usize::from(self.format.channels)
    }

    fn pending_frames(&self) -> u64 {
        ((self.pending.len() - self.pending_pos) / self.channels()) as u64
    }

    /// Pushes queued samples into the ring. True when everything was pushed.
    fn push_pending(&mut self) -> bool {
        let ch = self.channels();
        let remaining = self.pending.len() - self.pending_pos;
        if remaining == 0 {
            return true;
        }
        let space = self.producer.slots() / ch * ch;
        let n = remaining.min(space);
        if n > 0
            && let Ok(chunk) = self.producer.write_chunk_uninit(n)
        {
            let from = self.pending_pos;
            chunk.fill_from_iter(self.pending[from..from + n].iter().copied());
            self.pending_pos += n;
            self.pushed += (n / ch) as u64;
        }
        if self.pending_pos == self.pending.len() {
            self.pending.clear();
            self.pending_pos = 0;
            true
        } else {
            false
        }
    }

    /// Runs one decoded block through gain, channel mapping and resampling.
    fn process(&mut self, block: &[f64], gain: Option<f64>) -> Result<(), String> {
        let input: &[f64] = match gain {
            Some(g) => {
                self.scratch.clear();
                self.scratch.extend(block.iter().map(|s| s * g));
                &self.scratch
            }
            None => block,
        };
        let mapped: &[f64] = if self.map.is_identity() {
            input
        } else {
            self.mapped.clear();
            self.map.apply(input, &mut self.mapped);
            &self.mapped
        };
        match &mut self.resampler {
            Some(r) => r.process(mapped, &mut self.pending),
            None => {
                self.pending.extend_from_slice(mapped);
                Ok(())
            }
        }
    }

    fn flush_resampler(&mut self) {
        if let Some(r) = &mut self.resampler
            && let Err(e) = r.flush(&mut self.pending)
        {
            tracing::warn!("{e}");
        }
    }

    /// Clears everything after an output flush.
    fn reset_pipeline(&mut self) {
        self.pending.clear();
        self.pending_pos = 0;
        if let Some(r) = &mut self.resampler {
            r.reset();
        }
        self.pushed = self.stream.shared.consumed.load(Ordering::Acquire);
    }

    fn played(&self) -> u64 {
        self.stream.shared.played.load(Ordering::Acquire)
    }
}

struct Control {
    backend: Backend,
    output: OutputConfig,
    replaygain: ReplayGainMode,
    gapless: bool,
    volume: f32,
    events: Sender<Event>,

    items: Vec<QueueItem>,
    /// Queue state of the audible track.
    queue: Queue,
    /// Queue state of the track being decoded (may be one ahead).
    dec_queue: Queue,
    state: PlaybackState,

    dec: Option<Active>,
    /// The decoder's track already has a segment in the stream.
    dec_started: bool,
    out: Option<Out>,
    segments: VecDeque<Segment>,
    /// Decoded next track waiting for the device to drain (format change or
    /// gapless off).
    pending_switch: Option<(Active, Queue)>,
    /// The decoder reached the end of the queue.
    ended: bool,
    /// Position when there is no stream (paused before playing, stopped).
    idle_position: Duration,
    idle_index: Option<usize>,

    last_position: Instant,
    last_underruns: u64,
    last_signal: Option<SignalPath>,
    recovering_since: Option<Instant>,
}

impl Control {
    fn new(config: EngineConfig, events: Sender<Event>) -> Self {
        let queue = Queue::new(Rng::from_time());
        Self {
            backend: config.backend,
            output: config.output,
            replaygain: config.replaygain,
            gapless: config.gapless,
            volume: config.volume.clamp(0.0, 1.0),
            events,
            items: Vec::new(),
            dec_queue: queue.clone(),
            queue,
            state: PlaybackState::Stopped,
            dec: None,
            dec_started: false,
            out: None,
            segments: VecDeque::new(),
            pending_switch: None,
            ended: false,
            idle_position: Duration::ZERO,
            idle_index: None,
            last_position: Instant::now(),
            last_underruns: 0,
            last_signal: None,
            recovering_since: None,
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn run(mut self, commands: Receiver<Command>) {
        loop {
            let busy = self.state == PlaybackState::Playing && self.out.is_some();
            let first = if busy {
                match commands.recv_timeout(PUMP_INTERVAL) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                // Idle: block until a command arrives (no CPU while paused).
                match commands.recv() {
                    Ok(c) => Some(c),
                    Err(_) => break,
                }
            };
            if let Some(c) = first {
                if !self.handle(c) {
                    break;
                }
                let mut quit = false;
                while let Ok(c) = commands.try_recv() {
                    if !self.handle(c) {
                        quit = true;
                        break;
                    }
                }
                if quit {
                    break;
                }
            }
            if self.state == PlaybackState::Playing {
                self.pump();
            }
        }
        self.out = None;
    }

    /// Returns false on shutdown.
    fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::SetQueue {
                items,
                start,
                position,
                play,
            } => {
                self.items = items;
                self.queue.set_items(self.items.len(), start);
                match self.queue.current() {
                    Some(i) => self.start_at(i, position, play, true),
                    None => self.stop(),
                }
            }
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::TogglePause => {
                if self.state == PlaybackState::Playing {
                    self.pause();
                } else {
                    self.play();
                }
            }
            Command::Stop => self.stop(),
            Command::Seek(position) => {
                if let Some(i) = self.queue.current() {
                    let play = self.state == PlaybackState::Playing;
                    self.start_at(i, position, play, false);
                }
            }
            Command::Next => {
                if let Some(i) = self.queue.next_manual() {
                    let play = self.state != PlaybackState::Paused;
                    self.start_at(i, Duration::ZERO, play, true);
                }
            }
            Command::Prev => {
                let elapsed = self.position();
                let play = self.state != PlaybackState::Paused;
                match self.queue.prev(elapsed) {
                    PrevAction::Restart(i) => self.start_at(i, Duration::ZERO, play, false),
                    PrevAction::Play(i) => self.start_at(i, Duration::ZERO, play, true),
                    PrevAction::Nothing => {}
                }
            }
            Command::PlayIndex(i) => {
                if self.queue.jump_to(i).is_some() {
                    self.start_at(i, Duration::ZERO, true, true);
                }
            }
            Command::Enqueue { items, next } => self.enqueue(items, next),
            Command::SetVolume(v) => {
                let old = self.volume;
                self.volume = v.clamp(0.0, 1.0);
                if let Some(out) = &self.out {
                    out.stream.set_volume(self.volume);
                }
                if (old >= 1.0) != (self.volume >= 1.0) {
                    self.refresh_signal_path();
                }
            }
            Command::SetShuffle(on) => {
                self.queue.set_shuffle(on);
                self.after_queue_mode_change();
            }
            Command::SetRepeat(mode) => {
                self.queue.set_repeat(mode);
                self.after_queue_mode_change();
            }
            Command::SetOutput(config) => {
                if config != self.output {
                    self.output = config;
                    self.reopen_at_current_position();
                }
            }
            Command::SetReplayGain(mode) => {
                if mode != self.replaygain {
                    self.replaygain = mode;
                    if self.state == PlaybackState::Playing {
                        self.reopen_at_current_position();
                    }
                }
            }
            Command::SetGapless(on) => self.gapless = on,
            Command::ListDevices => match self.backend.list_devices() {
                Ok(d) => self.emit(Event::Devices(d)),
                Err(e) => self.emit(Event::Error(format!("cannot list audio devices: {e}"))),
            },
            Command::Shutdown => return false,
        }
        true
    }

    // ---- transport -------------------------------------------------------

    fn play(&mut self) {
        match self.state {
            PlaybackState::Playing => {}
            PlaybackState::Paused => {
                // Resume in place whenever the stream holds audio for us, even if
                // the decoder already finished the file (the tail is still buffered).
                let resumable = self.out.is_some()
                    && (!self.segments.is_empty() || self.pending_switch.is_some());
                if resumable {
                    if let Some(out) = &self.out {
                        out.stream.set_running(true);
                    }
                    self.set_state(PlaybackState::Playing);
                } else if let Some(i) = self.idle_index.or(self.queue.current()) {
                    let pos = self.idle_position;
                    self.start_at(i, pos, true, false);
                }
            }
            PlaybackState::Stopped => {
                if let Some(i) = self.queue.current() {
                    self.start_at(i, Duration::ZERO, true, true);
                }
            }
        }
    }

    fn pause(&mut self) {
        if self.state != PlaybackState::Playing {
            return;
        }
        if let Some(out) = &self.out {
            out.stream.set_running(false);
        }
        if let Some(seg) = self.segments.front() {
            self.idle_index = Some(seg.index);
        }
        if self.holds_exclusive_device()
            && let Some(out) = self.out.take()
        {
            // An exclusive stream locks the device for every other program,
            // even while silent: close it and reopen the file on resume.
            // Closing first stops the device thread, so the played count
            // (and the resume point) is final.
            let shared = Arc::clone(&out.stream.shared);
            drop(out);
            self.idle_position = match self.segments.front() {
                Some(seg) => {
                    let played = shared
                        .played
                        .load(Ordering::Acquire)
                        .saturating_sub(seg.start);
                    seg.offset + Duration::from_secs_f64(played as f64 / f64::from(seg.rate.max(1)))
                }
                None => self.idle_position,
            };
            self.set_state(PlaybackState::Paused);
            if let Some(index) = self.idle_index.or(self.queue.current()) {
                let position = self.idle_position;
                self.start_at(index, position, false, false);
            } else {
                self.emit_position();
            }
            return;
        }
        self.idle_position = self.position();
        self.set_state(PlaybackState::Paused);
        self.emit_position();
    }

    fn holds_exclusive_device(&self) -> bool {
        self.out
            .as_ref()
            .is_some_and(|o| o.stream.info.mode == OutputMode::Exclusive)
    }

    fn stop(&mut self) {
        self.out = None;
        self.dec = None;
        self.dec_started = false;
        self.pending_switch = None;
        self.segments.clear();
        self.ended = false;
        self.idle_position = Duration::ZERO;
        self.idle_index = self.queue.current();
        self.set_state(PlaybackState::Stopped);
        self.emit_position();
    }

    fn set_state(&mut self, state: PlaybackState) {
        if self.state != state {
            self.state = state;
            self.emit(Event::StateChanged(state));
        }
    }

    /// Starts item `index` (already current in `self.queue`) at `position`.
    /// Skips unplayable files forward. `announce` sends `TrackChanged`.
    fn start_at(&mut self, index: usize, position: Duration, play: bool, announce: bool) {
        self.segments.clear();
        self.pending_switch = None;
        self.ended = false;
        self.dec = None;
        self.dec_started = false;
        if let Some(out) = &mut self.out {
            out.stream.flush();
            out.reset_pipeline();
            out.stream.shared.draining.store(false, Ordering::Release);
        }

        // Open the requested track, skipping files that fail.
        let mut index = index;
        let mut position = position;
        let mut attempts = 0;
        let active = loop {
            match self.open_track(index, position) {
                Ok(a) => break a,
                Err(reason) => {
                    self.skipped(index, reason);
                    attempts += 1;
                    position = Duration::ZERO;
                    let next = if attempts <= self.items.len() {
                        self.queue.advance_auto()
                    } else {
                        None
                    };
                    match next {
                        Some(n) if n != index || self.queue.repeat() == RepeatMode::One => {
                            if self.queue.repeat() == RepeatMode::One && n == index {
                                // Repeat-one on a broken file: give up.
                                self.stop();
                                return;
                            }
                            index = n;
                        }
                        _ => {
                            self.stop();
                            self.emit(Event::EndOfQueue);
                            return;
                        }
                    }
                }
            }
        };

        self.dec_queue = self.queue.clone();
        if announce || attempts > 0 {
            self.emit(Event::TrackChanged {
                index,
                item: self.items[index].clone(),
                info: active.decoder.info().clone(),
            });
            self.emit_queue();
        }
        self.idle_index = Some(index);
        self.idle_position = active.start_offset;

        if !play {
            // Keep a shared stream (if it fits) but never sit paused on an
            // exclusive one: that would lock the device for other programs.
            let keep = !self.holds_exclusive_device()
                && self
                    .out
                    .as_ref()
                    .is_some_and(|o| self.stream_fits(o, &active));
            if !keep {
                self.out = None;
            }
            if self.out.is_some() {
                self.begin_segment(active, None, true);
            } else {
                self.dec = Some(active);
            }
            if self.state == PlaybackState::Playing || self.state == PlaybackState::Stopped {
                self.set_state(PlaybackState::Paused);
            }
            self.emit_position();
            return;
        }

        if let Err(e) = self.ensure_stream(&active) {
            self.emit(Event::Error(e));
            self.dec = Some(active);
            self.set_state(PlaybackState::Paused);
            self.emit_position();
            return;
        }
        self.begin_segment(active, None, true);
        if let Some(out) = &self.out {
            out.stream.set_running(true);
        }
        self.set_state(PlaybackState::Playing);
        self.emit_position();
        self.pump();
    }

    fn reopen_at_current_position(&mut self) {
        let Some(index) = self.queue.current() else {
            return;
        };
        let position = self.position();
        let play = self.state == PlaybackState::Playing;
        self.out = None;
        if play || self.state == PlaybackState::Paused {
            self.start_at(index, position, play, false);
        }
    }

    fn after_queue_mode_change(&mut self) {
        // If the next track is already buffered it may no longer be the right
        // one: rewind the decoder to the audible track.
        let ahead = self.dec.as_ref().map(|d| d.index) != self.queue.current()
            || self.pending_switch.is_some()
            || self.ended
            || self.segments.len() > 1;
        if ahead && self.state == PlaybackState::Playing {
            self.reopen_at_current_position();
        } else {
            self.dec_queue = self.queue.clone();
        }
        self.emit_queue();
    }

    fn enqueue(&mut self, items: Vec<QueueItem>, next: bool) {
        if items.is_empty() {
            return;
        }
        if self.queue.current().is_none() {
            // Nothing queued: these become the queue, ready to play.
            self.items = items;
            self.queue.set_items(self.items.len(), 0);
            if let Some(i) = self.queue.current() {
                self.start_at(i, Duration::ZERO, false, true);
            }
            return;
        }
        let count = items.len();
        self.items.extend(items);
        self.queue.insert(count, next);
        // The decoder may already be on the following track (gapless
        // pre-decode) or done with the queue; its queue snapshots then miss
        // the new items.
        let ahead = self.dec.as_ref().map(|d| d.index) != self.queue.current()
            || self.pending_switch.is_some()
            || self.ended
            || self.segments.len() > 1;
        if !ahead {
            self.dec_queue = self.queue.clone();
        } else if !next && !self.ended {
            // Appending leaves what is already buffered correct: add the items
            // to every snapshot instead of interrupting the audio.
            self.dec_queue.insert(count, false);
            for seg in &mut self.segments {
                if let Some(q) = seg.queue.as_mut() {
                    q.insert(count, false);
                }
            }
            if let Some((_, q)) = self.pending_switch.as_mut() {
                q.insert(count, false);
            }
        } else if self.state != PlaybackState::Stopped {
            // Rare: "Play next" in the last second of a track, or adding after
            // the decoder finished the queue. Re-decode from the audible spot.
            self.reopen_at_current_position();
        }
        self.emit_queue();
    }

    fn emit_queue(&mut self) {
        let event = Event::QueueChanged {
            order: self.queue.order().to_vec(),
            current: self.queue.current(),
            shuffle: self.queue.shuffle(),
            repeat: self.queue.repeat(),
        };
        self.emit(event);
    }

    fn skipped(&self, index: usize, reason: String) {
        let path = self
            .items
            .get(index)
            .map(|i| i.path.clone())
            .unwrap_or_default();
        tracing::warn!("skipping {}: {reason}", path.display());
        self.emit(Event::TrackSkipped {
            index,
            path,
            reason,
        });
    }

    // ---- tracks and streams ---------------------------------------------

    fn open_track(&self, index: usize, position: Duration) -> Result<Active, String> {
        let item = self.items.get(index).ok_or("no such queue item")?;
        if !item.path.exists() {
            return Err("file not found".into());
        }
        let mut decoder = Decoder::open(&item.path)?;
        let mut offset = Duration::ZERO;
        if !position.is_zero() {
            offset = decoder.seek(position)?;
        }
        let gain = replaygain_factor(&decoder.info().replaygain, self.replaygain);
        Ok(Active {
            index,
            decoder,
            gain,
            start_offset: offset,
        })
    }

    /// The format to open the device for: the file's, or the fixed output
    /// rate when one is set (exclusive mode resamples to it).
    fn source_format(&self, active: &Active) -> SourceFormat {
        let info = active.decoder.info();
        SourceFormat {
            rate: self.output.rate.unwrap_or(info.sample_rate),
            channels: info.channels,
            bits: info.bits_per_sample,
        }
    }

    fn stream_fits(&self, out: &Out, active: &Active) -> bool {
        match out.stream.info.mode {
            OutputMode::Exclusive => {
                self.output.exclusive
                    && exclusive_stream_fits(&out.format, &self.source_format(active))
            }
            // Shared mode takes anything (resampling as needed). A shared
            // *fallback* stream only carries further tracks at the rate the
            // device refused; any other rate tries exclusive mode again.
            OutputMode::Shared => {
                !self.output.exclusive
                    || out.fallback_rate == Some(active.decoder.info().sample_rate)
            }
        }
    }

    /// Makes sure an output stream that can play `active` is open.
    fn ensure_stream(&mut self, active: &Active) -> Result<(), String> {
        if let Some(out) = &self.out
            && self.stream_fits(out, active)
        {
            return Ok(());
        }
        self.out = None;
        let req = OpenRequest {
            device_id: self.output.device_id.clone(),
            exclusive: self.output.exclusive,
            source: self.source_format(active),
            volume: self.volume,
        };
        let source_rate = req.source.rate;
        let (stream, producer) = OutputStream::open(&self.backend, req)?;
        let format = stream.info.format;
        let fallback_rate = (stream.info.mode == OutputMode::Shared && self.output.exclusive)
            .then_some(source_rate);
        tracing::info!(
            "output: {} ({:?}, {format})",
            stream.info.device_name,
            stream.info.mode
        );
        self.out = Some(Out {
            stream,
            producer,
            format,
            fallback_rate,
            pushed: 0,
            resampler: None,
            map: ChannelMap::new(1, 1),
            pending: Vec::new(),
            pending_pos: 0,
            scratch: Vec::new(),
            mapped: Vec::new(),
        });
        self.recovering_since = None;
        Ok(())
    }

    /// Starts feeding `active` into the open stream at the current write position.
    fn begin_segment(&mut self, active: Active, queue: Option<Queue>, announced: bool) {
        let Some(out) = self.out.as_mut() else {
            self.dec = Some(active);
            return;
        };
        let info = active.decoder.info().clone();
        out.map = ChannelMap::new(usize::from(info.channels), out.channels());
        let need =
            (info.sample_rate != out.format.rate).then_some((info.sample_rate, out.format.rate));
        let keep = matches!((&out.resampler, need), (Some(r), Some(n)) if r.rates() == n);
        if !keep {
            out.flush_resampler();
            out.resampler = match need {
                Some((from, to)) => match Resampler::new(from, to, out.channels()) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        let _ = self.events.send(Event::Error(e));
                        None
                    }
                },
                None => None,
            };
        }
        out.stream.shared.draining.store(false, Ordering::Release);
        let start = out.pushed + out.pending_frames();
        let signal = Self::signal_path(out, &info, active.gain, self.volume);
        let rate = out.format.rate;
        if announced {
            self.emit_signal(signal.clone());
        }
        self.segments.push_back(Segment {
            start,
            index: active.index,
            offset: active.start_offset,
            rate,
            info,
            signal,
            queue,
            announced,
        });
        self.dec = Some(active);
        self.dec_started = true;
    }

    fn signal_path(
        out: &Out,
        info: &TrackInfo,
        gain: Option<(f64, f32)>,
        volume: f32,
    ) -> SignalPath {
        let resampled =
            (info.sample_rate != out.format.rate).then_some((info.sample_rate, out.format.rate));
        let mode = out.stream.info.mode;
        let channels_exact = info.channels == out.format.channels
            || (info.channels == 1 && out.format.channels == 2);
        SignalPath {
            source: info.clone(),
            device_name: out.stream.info.device_name.clone(),
            mode,
            output: out.format,
            resampled,
            exclusive_fallback: out.stream.info.exclusive_fallback.clone(),
            replaygain_db: gain.map(|(_, db)| db),
            volume_attenuates: volume < 1.0,
            bit_perfect: mode == OutputMode::Exclusive
                && resampled.is_none()
                && channels_exact
                && carries_exactly(out.format.sample, info.bits_per_sample)
                && gain.is_none()
                && volume >= 1.0,
        }
    }

    fn refresh_signal_path(&mut self) {
        let volume = self.volume;
        let Some(out) = &self.out else { return };
        let Some(seg) = self.segments.front_mut() else {
            return;
        };
        let gain = seg
            .signal
            .replaygain_db
            .map(|db| (10f64.powf(f64::from(db) / 20.0), db));
        seg.signal = Self::signal_path(out, &seg.info, gain, volume);
        let signal = seg.signal.clone();
        self.emit_signal(signal);
    }

    /// Sends the signal path when it changed since the last one sent.
    fn emit_signal(&mut self, signal: SignalPath) {
        if self.last_signal.as_ref() != Some(&signal) {
            self.last_signal = Some(signal.clone());
            self.emit(Event::SignalPath(signal));
        }
    }

    // ---- the pump ----------------------------------------------------------

    fn pump(&mut self) {
        if self.check_stream_failure() {
            return;
        }
        self.fill();
        self.handle_pending_switch();
        self.update_audible();
        self.check_end();
        if self.last_position.elapsed() >= POSITION_INTERVAL {
            self.emit_position();
            self.report_underruns();
        }
    }

    /// Decodes until the ring buffer is full.
    fn fill(&mut self) {
        loop {
            let Some(out) = self.out.as_mut() else { return };
            if !out.push_pending() {
                return; // ring full
            }
            if self.ended || self.pending_switch.is_some() || !self.dec_started {
                if self.ended {
                    out.stream.shared.draining.store(true, Ordering::Release);
                }
                return;
            }
            let Some(active) = self.dec.as_mut() else {
                return;
            };
            let gain = active.gain.map(|(g, _)| g);
            let index = active.index;
            let step = match active.decoder.next_block() {
                Ok(Some(block)) => match out.process(block, gain) {
                    Ok(()) => Step::Continue,
                    Err(e) => Step::Error(e),
                },
                Ok(None) => Step::TrackDone,
                Err(e) => Step::TrackFailed(e),
            };
            match step {
                Step::Continue => {}
                Step::Error(e) => self.emit(Event::Error(e)),
                Step::TrackDone => self.on_track_decoded(),
                Step::TrackFailed(e) => {
                    self.skipped(index, e);
                    self.on_track_decoded();
                }
            }
        }
    }

    /// The decoder finished its track: queue up the next one.
    fn on_track_decoded(&mut self) {
        self.dec = None;
        self.dec_started = false;
        let mut attempts = 0;
        loop {
            let next = if attempts <= self.items.len() {
                self.dec_queue.advance_auto()
            } else {
                None
            };
            let Some(index) = next else {
                // End of the queue: flush the resampler tail and let it drain.
                if let Some(out) = self.out.as_mut() {
                    out.flush_resampler();
                }
                self.ended = true;
                return;
            };
            match self.open_track(index, Duration::ZERO) {
                Ok(active) => {
                    let snapshot = self.dec_queue.clone();
                    let fits = self
                        .out
                        .as_ref()
                        .is_some_and(|o| self.stream_fits(o, &active));
                    if self.gapless && fits {
                        self.begin_segment(active, Some(snapshot), false);
                    } else {
                        if let Some(out) = self.out.as_mut() {
                            out.flush_resampler();
                            out.stream.shared.draining.store(true, Ordering::Release);
                        }
                        self.pending_switch = Some((active, snapshot));
                    }
                    return;
                }
                Err(reason) => {
                    self.skipped(index, reason);
                    attempts += 1;
                    if self.dec_queue.repeat() == RepeatMode::One {
                        attempts = usize::MAX;
                    }
                }
            }
        }
    }

    /// After the device drained, switch to a track that needed a new stream
    /// (or a pause, with gapless off).
    fn handle_pending_switch(&mut self) {
        let drained = match &self.out {
            Some(out) => out.pending_frames() == 0 && out.played() >= out.pushed,
            None => true,
        };
        if !drained || self.pending_switch.is_none() {
            return;
        }
        let Some((active, snapshot)) = self.pending_switch.take() else {
            return;
        };
        self.segments.clear();
        let fits = self
            .out
            .as_ref()
            .is_some_and(|o| self.stream_fits(o, &active));
        if !fits {
            self.out = None;
        }
        let item_index = active.index;
        if let Err(e) = self.ensure_stream(&active) {
            self.emit(Event::Error(e));
            self.stop();
            return;
        }
        self.queue = snapshot.clone();
        self.dec_queue = snapshot;
        self.emit(Event::TrackChanged {
            index: item_index,
            item: self.items[item_index].clone(),
            info: active.decoder.info().clone(),
        });
        self.emit_queue();
        self.begin_segment(active, None, true);
        if let Some(out) = &self.out {
            out.stream.set_running(true);
        }
        self.fill();
    }

    /// Tracks which segment is audible; announces gapless track changes.
    fn update_audible(&mut self) {
        let Some(out) = &self.out else { return };
        let played = out.played();
        // Announce every segment in order, even ones that finished between two
        // pump ticks (very short tracks), so no TrackChanged is ever lost.
        loop {
            self.announce_front();
            if self.segments.len() > 1 && self.segments[1].start <= played {
                self.segments.pop_front();
            } else {
                break;
            }
        }
    }

    fn announce_front(&mut self) {
        let Some(seg) = self.segments.front_mut() else {
            return;
        };
        if seg.announced {
            return;
        }
        seg.announced = true;
        let index = seg.index;
        let info = seg.info.clone();
        let signal = seg.signal.clone();
        if let Some(q) = seg.queue.take() {
            self.queue = q;
        }
        self.idle_index = Some(index);
        self.emit(Event::TrackChanged {
            index,
            item: self.items[index].clone(),
            info,
        });
        self.emit_signal(signal);
        self.emit_queue();
    }

    fn check_end(&mut self) {
        if !self.ended || self.pending_switch.is_some() {
            return;
        }
        let Some(out) = &self.out else { return };
        if out.pending_frames() > 0 || out.played() < out.pushed {
            return;
        }
        self.emit_position();
        self.out = None;
        self.dec = None;
        self.dec_started = false;
        self.segments.clear();
        self.ended = false;
        self.idle_position = Duration::ZERO;
        self.idle_index = self.queue.current();
        self.set_state(PlaybackState::Stopped);
        self.emit(Event::EndOfQueue);
    }

    /// Device lost or failed: report and try once to reopen at the same spot.
    fn check_stream_failure(&mut self) -> bool {
        let Some(out) = &self.out else { return false };
        if !out.stream.shared.failed.load(Ordering::Acquire) {
            return false;
        }
        let msg = out
            .stream
            .shared
            .take_error()
            .unwrap_or_else(|| "audio output failed".into());
        self.emit(Event::Error(msg));
        let retry = self
            .recovering_since
            .is_none_or(|t| t.elapsed() > Duration::from_secs(5));
        if retry {
            self.recovering_since = Some(Instant::now());
            self.reopen_at_current_position();
        } else {
            let position = self.position();
            self.out = None;
            self.idle_position = position;
            self.set_state(PlaybackState::Paused);
        }
        true
    }

    // ---- position ------------------------------------------------------------

    fn position(&self) -> Duration {
        match (&self.out, self.segments.front()) {
            (Some(out), Some(seg)) => {
                let played = out.played().saturating_sub(seg.start);
                seg.offset + Duration::from_secs_f64(played as f64 / f64::from(seg.rate.max(1)))
            }
            _ => self.idle_position,
        }
    }

    fn emit_position(&mut self) {
        self.last_position = Instant::now();
        let (index, duration) = match self.segments.front() {
            Some(seg) => (seg.index, seg.info.duration),
            None => match &self.dec {
                Some(a) => (a.index, a.decoder.info().duration),
                None => match self.idle_index {
                    Some(i) => (i, None),
                    None => return,
                },
            },
        };
        let mut position = self.position();
        if let Some(d) = duration {
            position = position.min(d);
        }
        self.emit(Event::Position {
            index,
            position,
            duration,
        });
    }

    fn report_underruns(&mut self) {
        if let Some(out) = &self.out {
            let n = out.stream.shared.underruns.load(Ordering::Relaxed);
            if n != self.last_underruns {
                self.last_underruns = n;
                self.emit(Event::Underruns(n));
            }
        }
    }
}
