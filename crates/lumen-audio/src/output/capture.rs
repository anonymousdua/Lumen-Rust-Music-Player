//! An in-memory output device. It negotiates like a real device (it accepts a
//! configurable set of exclusive formats) and records every sample it is
//! given, exactly as the device would receive it. It plays faster than real
//! time and never inserts silence, so tests can compare output to input
//! sample-for-sample.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;

use super::{OpenRequest, OpenResult, OpenedInfo, Renderer, StreamShared, ring_for};
use crate::dsp::read_int_sample;
use crate::negotiate::{Plan, plan, shared_plan};
use crate::protocol::{OutputMode, SampleFormat, StreamFormat};

type AcceptFn = dyn Fn(&StreamFormat) -> bool + Send + Sync;

pub struct CaptureDevice {
    pub name: String,
    /// Shared-mode mix format.
    pub mix: StreamFormat,
    accepts: Box<AcceptFn>,
    /// Simulates `IAudioClient::Initialize` failing in exclusive mode
    /// (device in use, exclusive mode disabled, …).
    pub exclusive_init_error: Option<String>,
    log: Mutex<Vec<CapturedStream>>,
}

/// Everything one opened stream received.
#[derive(Clone, Debug, Default)]
pub struct CapturedStream {
    pub info: Option<OpenedInfo>,
    /// Samples as the device got them: integer formats as left-justified
    /// `i32` values, float formats as their value.
    pub samples: Vec<f64>,
    /// Sample indices where a flush (seek, skip) happened.
    pub flushes: Vec<usize>,
    /// The stream was closed (the device is free again).
    pub closed: bool,
}

impl CaptureDevice {
    pub fn new(
        name: &str,
        mix: StreamFormat,
        accepts: impl Fn(&StreamFormat) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.to_string(),
            mix,
            accepts: Box::new(accepts),
            exclusive_init_error: None,
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn with_exclusive_init_error(mut self, error: &str) -> Self {
        self.exclusive_init_error = Some(error.to_string());
        self
    }

    /// Streams opened so far, in order.
    pub fn streams(&self) -> Vec<CapturedStream> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    fn with_stream(&self, index: usize, f: impl FnOnce(&mut CapturedStream)) {
        if let Ok(mut log) = self.log.lock()
            && let Some(s) = log.get_mut(index)
        {
            f(s);
        }
    }
}

pub(crate) fn run(
    dev: Arc<CaptureDevice>,
    req: OpenRequest,
    shared: Arc<StreamShared>,
    result: Sender<OpenResult>,
) {
    let mut chosen = plan(&req.source, req.exclusive, dev.mix, |f| (dev.accepts)(f));
    if let (Plan::Exclusive { .. }, Some(err)) = (&chosen, &dev.exclusive_init_error) {
        chosen = shared_plan(
            &req.source,
            dev.mix,
            Some(format!("Exclusive mode failed: {err}")),
        );
    }
    let info = match chosen {
        Plan::Exclusive { format, exact } => OpenedInfo {
            device_name: dev.name.clone(),
            mode: OutputMode::Exclusive,
            format,
            exact,
            exclusive_fallback: None,
        },
        Plan::Shared {
            format, fallback, ..
        } => OpenedInfo {
            device_name: dev.name.clone(),
            mode: OutputMode::Shared,
            format,
            exact: false,
            exclusive_fallback: fallback,
        },
    };

    let (producer, consumer) = ring_for(&info.format);
    let mut renderer = Renderer::new(consumer, info.format, req.volume);
    let index = match dev.log.lock() {
        Ok(mut log) => {
            log.push(CapturedStream {
                info: Some(info.clone()),
                ..Default::default()
            });
            log.len() - 1
        }
        Err(_) => 0,
    };
    let format = info.format;
    if result.send(Ok((info, producer))).is_err() {
        return;
    }

    let bpf = renderer.bytes_per_frame();
    let mut bytes = vec![0u8; 4096 * bpf];
    let mut decoded: Vec<f64> = Vec::with_capacity(4096 * usize::from(format.channels));
    let sample_bytes = format.sample.container_bytes();
    let mut ack = 0;

    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        let req_flush = shared.flush_req.load(Ordering::Acquire);
        if req_flush != ack {
            renderer.drain();
            let played = shared.consumed.load(Ordering::Acquire);
            shared.played.store(played, Ordering::Release);
            dev.with_stream(index, |s| s.flushes.push(s.samples.len()));
            ack = req_flush;
            shared.flush_ack.store(ack, Ordering::Release);
            continue;
        }
        if !shared.running.load(Ordering::Acquire) {
            std::thread::park_timeout(Duration::from_millis(20));
            continue;
        }
        let frames = renderer.available_frames().min(4096);
        if frames == 0 {
            std::thread::park_timeout(Duration::from_micros(300));
            continue;
        }
        let out = &mut bytes[..frames * bpf];
        let got = renderer.render(out, shared.volume());
        decoded.clear();
        for s in out[..got * bpf].chunks_exact(sample_bytes) {
            decoded.push(match format.sample {
                SampleFormat::Int { .. } => f64::from(read_int_sample(s)),
                SampleFormat::Float32 => f64::from(f32::from_le_bytes([s[0], s[1], s[2], s[3]])),
            });
        }
        dev.with_stream(index, |s| s.samples.extend_from_slice(&decoded));
        let total = shared.consumed.fetch_add(got as u64, Ordering::AcqRel) + got as u64;
        shared.played.store(total, Ordering::Release);
    }
    dev.with_stream(index, |s| s.closed = true);
}
