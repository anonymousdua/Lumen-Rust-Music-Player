//! Device output: one thread per open stream, fed by a lock-free ring buffer.
//!
//! The control thread (decoder) is the only producer, the device thread the
//! only consumer. They share nothing else but atomics; the device thread never
//! locks, allocates or waits on the control thread while playing.
//!
//! Handshakes (pause, flush) go through atomics plus `Thread::unpark`.

pub mod capture;
#[cfg(windows)]
pub mod wasapi;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::dsp::{Converter, GainRamp};
use crate::negotiate::SourceFormat;
use crate::protocol::{DeviceInfo, OutputMode, StreamFormat};

/// Ring buffer length between decoder and device.
pub const RING_SECONDS: f64 = 0.5;

/// Where audio goes.
#[derive(Clone)]
pub enum Backend {
    /// Real WASAPI endpoints.
    Wasapi,
    /// In-memory device for tests and diagnostics; records everything played.
    Capture(Arc<capture::CaptureDevice>),
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backend::Wasapi => write!(f, "Wasapi"),
            Backend::Capture(_) => write!(f, "Capture"),
        }
    }
}

impl Backend {
    pub fn list_devices(&self) -> Result<Vec<DeviceInfo>, String> {
        match self {
            #[cfg(windows)]
            Backend::Wasapi => wasapi::list_devices(),
            #[cfg(not(windows))]
            Backend::Wasapi => Ok(Vec::new()),
            Backend::Capture(dev) => Ok(vec![DeviceInfo {
                id: "capture".into(),
                name: dev.name.clone(),
                is_default: true,
            }]),
        }
    }
}

/// What the control thread asks for.
#[derive(Clone, Debug)]
pub struct OpenRequest {
    pub device_id: Option<String>,
    pub exclusive: bool,
    pub source: SourceFormat,
    /// Initial digital volume.
    pub volume: f32,
}

/// What the device thread actually opened.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenedInfo {
    pub device_name: String,
    pub mode: OutputMode,
    pub format: StreamFormat,
    /// Exclusive mode and the format carries the source unchanged.
    pub exact: bool,
    /// Why exclusive mode was requested but not used.
    pub exclusive_fallback: Option<String>,
}

/// State shared between control and device threads.
pub struct StreamShared {
    /// Play (true) or pause (false).
    pub running: AtomicBool,
    pub shutdown: AtomicBool,
    /// No more samples will be pushed (end of queue): silence is not an underrun.
    pub draining: AtomicBool,
    /// Volume as `f32` bits.
    volume: AtomicU32,
    pub flush_req: AtomicU64,
    pub flush_ack: AtomicU64,
    /// Frames taken from the ring buffer so far.
    pub consumed: AtomicU64,
    /// Frames from the ring buffer that the device has actually played.
    pub played: AtomicU64,
    pub underruns: AtomicU64,
    pub failed: AtomicBool,
    error: Mutex<Option<String>>,
}

impl StreamShared {
    pub fn new(volume: f32) -> Self {
        Self {
            running: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            volume: AtomicU32::new(volume.to_bits()),
            flush_req: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            played: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            error: Mutex::new(None),
        }
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        self.volume
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Called by the device thread when it gives up (device removed, …).
    pub fn fail(&self, message: String) {
        if let Ok(mut e) = self.error.lock() {
            *e = Some(message);
        }
        self.failed.store(true, Ordering::Release);
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut e| e.take())
    }
}

/// Pulls frames from the ring buffer and writes device bytes. Used by every
/// backend on its device thread; never allocates after construction.
pub struct Renderer {
    consumer: Consumer<f64>,
    converter: Converter,
    gain: GainRamp,
    channels: usize,
    bytes_per_frame: usize,
    ramp_frames: usize,
}

impl Renderer {
    pub fn new(consumer: Consumer<f64>, format: StreamFormat, volume: f32) -> Self {
        let channels = usize::from(format.channels);
        let converter = Converter::new(format.sample, channels);
        Self {
            bytes_per_frame: converter.bytes_per_frame(),
            converter,
            consumer,
            gain: GainRamp::new(f64::from(volume)),
            channels,
            // ~5 ms volume ramp.
            ramp_frames: (format.rate / 200).max(1) as usize,
        }
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.bytes_per_frame
    }

    pub fn available_frames(&self) -> usize {
        self.consumer.slots() / self.channels
    }

    /// Fills `out` (whole frames) from the ring, padding with silence.
    /// Returns the number of real (non-silent) frames written.
    pub fn render(&mut self, out: &mut [u8], volume: f32) -> usize {
        self.gain.set_target(f64::from(volume), self.ramp_frames);
        let frames = out.len() / self.bytes_per_frame;
        let take = frames.min(self.available_frames());
        if take > 0
            && let Ok(chunk) = self.consumer.read_chunk(take * self.channels)
        {
            let (a, b) = chunk.as_slices();
            let split = a.len() / self.channels * self.bytes_per_frame;
            let end = take * self.bytes_per_frame;
            self.converter.write(a, &mut self.gain, &mut out[..split]);
            self.converter
                .write(b, &mut self.gain, &mut out[split..end]);
            chunk.commit_all();
        }
        out[take * self.bytes_per_frame..].fill(0);
        take
    }

    /// Discards everything buffered (flush after seek / skip).
    pub fn drain(&mut self) {
        let n = self.consumer.slots();
        if let Ok(chunk) = self.consumer.read_chunk(n) {
            chunk.commit_all();
        }
    }
}

/// Ring buffer length in frames for a stream format.
pub fn ring_frames(format: &StreamFormat) -> usize {
    ((f64::from(format.rate) * RING_SECONDS) as usize).max(1024)
}

/// Creates the ring buffer for a stream format.
pub fn ring_for(format: &StreamFormat) -> (Producer<f64>, Consumer<f64>) {
    let channels = usize::from(format.channels).max(1);
    // A multiple of the channel count keeps both ring slices frame-aligned.
    RingBuffer::new(ring_frames(format) * channels)
}

/// Handle to an open output stream, owned by the control thread.
pub struct OutputStream {
    pub shared: Arc<StreamShared>,
    pub info: OpenedInfo,
    thread: Option<JoinHandle<()>>,
}

impl OutputStream {
    /// Opens a stream on `backend`. Blocks the calling (control) thread until
    /// the device thread has negotiated the format, at most a few seconds.
    pub fn open(backend: &Backend, req: OpenRequest) -> Result<(Self, Producer<f64>), String> {
        let shared = Arc::new(StreamShared::new(req.volume));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let thread_shared = shared.clone();
        let backend = backend.clone();
        let thread = std::thread::Builder::new()
            .name("lumen-output".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match backend {
                        #[cfg(windows)]
                        Backend::Wasapi => wasapi::run(req, thread_shared.clone(), tx),
                        #[cfg(not(windows))]
                        Backend::Wasapi => {
                            let _ = tx.send(Err("WASAPI is only available on Windows".into()));
                        }
                        Backend::Capture(dev) => capture::run(dev, req, thread_shared.clone(), tx),
                    }));
                if let Err(panic) = result {
                    let msg = panic_message(&panic);
                    tracing::error!("output thread panicked: {msg}");
                    thread_shared.fail(format!("output thread panicked: {msg}"));
                }
            })
            .map_err(|e| format!("cannot start output thread: {e}"))?;

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok((info, producer))) => Ok((
                Self {
                    shared,
                    info,
                    thread: Some(thread),
                },
                producer,
            )),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                shared.shutdown.store(true, Ordering::Release);
                thread.thread().unpark();
                Err("the audio device did not respond".into())
            }
        }
    }

    pub fn wake(&self) {
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }

    pub fn set_running(&self, running: bool) {
        self.shared.running.store(running, Ordering::Release);
        self.wake();
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared.set_volume(volume);
    }

    /// Empties the ring buffer and the device buffer. Returns once the device
    /// thread has done it (or after a timeout). The caller must not push while
    /// this runs.
    pub fn flush(&self) {
        let req = self.shared.flush_req.fetch_add(1, Ordering::AcqRel) + 1;
        self.wake();
        let deadline = Instant::now() + Duration::from_millis(500);
        while self.shared.flush_ack.load(Ordering::Acquire) < req {
            if Instant::now() > deadline || self.shared.failed.load(Ordering::Acquire) {
                tracing::warn!("output flush timed out");
                break;
            }
            std::thread::sleep(Duration::from_micros(500));
            self.wake();
        }
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// Result a device thread sends back after negotiating.
pub type OpenResult = Result<(OpenedInfo, Producer<f64>), String>;

pub(crate) fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}
