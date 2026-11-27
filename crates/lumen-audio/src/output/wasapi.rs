//! WASAPI output (exclusive and shared, event-driven).
//!
//! Everything COM-related is created, used and dropped on the device thread.
//! The thread joins MMCSS "Pro Audio" before touching the device.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use wasapi::{
    AudioClient, Device, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError,
    WaveFormat, calculate_period_100ns, initialize_mta,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED,
    AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED,
};

use super::{OpenRequest, OpenResult, OpenedInfo, Renderer, StreamShared, ring_for};
use crate::negotiate::{Plan, plan};
use crate::priority::ProAudioGuard;
use crate::protocol::{DeviceInfo, OutputMode, SampleFormat, StreamFormat};

/// Shared-mode device buffer (the engine period is chosen by Windows).
const SHARED_BUFFER_HNS: i64 = 400_000; // 40 ms
/// Consecutive missed device events before the device is considered gone.
const MAX_EVENT_TIMEOUTS: u32 = 10;

/// Lists active render endpoints.
pub fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    let _ = initialize_mta();
    let enumerator = DeviceEnumerator::new().map_err(err)?;
    let default_id = enumerator
        .get_default_device(Direction::Render)
        .and_then(|d| d.get_id())
        .ok();
    let collection = enumerator
        .get_device_collection(Direction::Render)
        .map_err(err)?;
    let mut out = Vec::new();
    for device in &collection {
        let device = device.map_err(err)?;
        let id = device.get_id().map_err(err)?;
        let name = device.get_friendlyname().unwrap_or_else(|_| id.clone());
        out.push(DeviceInfo {
            is_default: default_id.as_deref() == Some(id.as_str()),
            id,
            name,
        });
    }
    Ok(out)
}

pub(crate) fn run(req: OpenRequest, shared: Arc<StreamShared>, result: Sender<OpenResult>) {
    let _ = initialize_mta();
    let _mmcss = ProAudioGuard::join();

    let opened = match open(&req) {
        Ok(o) => o,
        Err(e) => {
            let _ = result.send(Err(e));
            wasapi::deinitialize();
            return;
        }
    };
    let Opened {
        client,
        info,
        buffer_frames,
    } = opened;

    let (producer, consumer) = ring_for(&info.format);
    let ring_frames = super::ring_frames(&info.format);
    let mut renderer = Renderer::new(consumer, info.format, req.volume);
    if result.send(Ok((info, producer))).is_err() {
        wasapi::deinitialize();
        return;
    }

    if let Err(e) = render_loop(&client, &mut renderer, &shared, buffer_frames, ring_frames) {
        tracing::error!("audio output stopped: {e}");
        shared.fail(e);
    }
    let _ = client.stop_stream();
    drop(client);
    wasapi::deinitialize();
}

struct Opened {
    client: AudioClient,
    info: OpenedInfo,
    buffer_frames: usize,
}

fn open(req: &OpenRequest) -> Result<Opened, String> {
    let enumerator = DeviceEnumerator::new().map_err(err)?;
    let device = match &req.device_id {
        Some(id) => match enumerator.get_device(id) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("device {id} unavailable ({e}), using the default device");
                enumerator
                    .get_default_device(Direction::Render)
                    .map_err(err)?
            }
        },
        None => enumerator
            .get_default_device(Direction::Render)
            .map_err(err)?,
    };
    let device_name = device
        .get_friendlyname()
        .unwrap_or_else(|_| "Audio device".into());

    let probe_client = device.get_iaudioclient().map_err(err)?;
    let mix_wave = probe_client.get_mixformat().map_err(err)?;
    let mix = from_wave(&mix_wave).ok_or("unsupported device mix format")?;

    // Ask the driver about each exclusive candidate, remembering the exact
    // WAVEFORMAT variant (mask, EX vs EXTENSIBLE) it accepted.
    let mut accepted: Option<WaveFormat> = None;
    let chosen = plan(&req.source, req.exclusive, mix, |f| {
        match probe_client.is_supported_exclusive_with_quirks(&to_wave(f)) {
            Ok(w) => {
                accepted = Some(w);
                true
            }
            Err(_) => false,
        }
    });
    drop(probe_client);

    let shared_info = |fallback: Option<String>| OpenedInfo {
        device_name: device_name.clone(),
        mode: OutputMode::Shared,
        format: mix,
        exact: false,
        exclusive_fallback: fallback,
    };

    match chosen {
        Plan::Exclusive { format, exact } => {
            let wave = accepted.unwrap_or_else(|| to_wave(&format));
            match init_exclusive(&device, &wave) {
                Ok((client, buffer_frames)) => Ok(Opened {
                    client,
                    info: OpenedInfo {
                        device_name: device_name.clone(),
                        mode: OutputMode::Exclusive,
                        format,
                        exact,
                        exclusive_fallback: None,
                    },
                    buffer_frames,
                }),
                Err(e) => {
                    let reason = describe_exclusive_error(&e);
                    tracing::warn!("exclusive mode failed: {reason}; using shared mode");
                    let (client, buffer_frames) = init_shared(&device, &mix_wave)?;
                    Ok(Opened {
                        client,
                        info: shared_info(Some(reason)),
                        buffer_frames,
                    })
                }
            }
        }
        Plan::Shared { fallback, .. } => {
            let (client, buffer_frames) = init_shared(&device, &mix_wave)?;
            Ok(Opened {
                client,
                info: shared_info(fallback),
                buffer_frames,
            })
        }
    }
}

fn init_exclusive(device: &Device, wave: &WaveFormat) -> Result<(AudioClient, usize), WasapiError> {
    let mut client = device.get_iaudioclient()?;
    let (default_period, _min) = client.get_device_period()?;
    // 128-byte alignment satisfies Intel HDA style drivers.
    let period = client.calculate_aligned_period_near(default_period, Some(128), wave)?;
    match client.initialize_client(
        wave,
        Direction::Render,
        StreamMode::EventsExclusive { period_hns: period },
        None,
    ) {
        Ok(()) => {}
        Err(WasapiError::Windows(e)) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            // Documented recovery: ask for the aligned size and initialise a new client.
            let frames = client.get_buffer_size()?;
            let aligned =
                calculate_period_100ns(i64::from(frames), i64::from(wave.get_samplespersec()));
            client = device.get_iaudioclient()?;
            client.initialize_client(
                wave,
                Direction::Render,
                StreamMode::EventsExclusive {
                    period_hns: aligned,
                },
                None,
            )?;
        }
        Err(e) => return Err(e),
    }
    let frames = client.get_buffer_size()? as usize;
    Ok((client, frames))
}

fn init_shared(device: &Device, mix: &WaveFormat) -> Result<(AudioClient, usize), String> {
    let mut client = device.get_iaudioclient().map_err(err)?;
    client
        .initialize_client(
            mix,
            Direction::Render,
            StreamMode::EventsShared {
                autoconvert: false,
                buffer_duration_hns: SHARED_BUFFER_HNS,
            },
            None,
        )
        .map_err(err)?;
    let frames = client.get_buffer_size().map_err(err)? as usize;
    Ok((client, frames))
}

fn render_loop(
    client: &AudioClient,
    renderer: &mut Renderer,
    shared: &StreamShared,
    buffer_frames: usize,
    ring_frames: usize,
) -> Result<(), String> {
    let event = client.set_get_eventhandle().map_err(err)?;
    let render_client = client.get_audiorenderclient().map_err(err)?;
    let bpf = renderer.bytes_per_frame();
    let mut bytes = vec![0u8; buffer_frames * bpf];

    let mut started = false;
    let mut ack = shared.flush_ack.load(Ordering::Acquire);
    // Frame counters since the last device reset.
    let mut written: u64 = 0; // all frames written to the device
    let mut audio_end: u64 = 0; // device frame index just after the last real frame
    let mut consumed: u64 = shared.consumed.load(Ordering::Acquire);
    let mut timeouts = 0u32;
    let mut prefill_since: Option<Instant> = None;
    let prefill_frames = buffer_frames.min(ring_frames / 2).max(1);

    let mut write = |renderer: &mut Renderer,
                     frames: usize,
                     written: &mut u64,
                     audio_end: &mut u64,
                     consumed: &mut u64|
     -> Result<usize, String> {
        if frames == 0 {
            return Ok(0);
        }
        let out = &mut bytes[..frames * bpf];
        let real = renderer.render(out, shared.volume());
        render_client.write_to_device(out, None).map_err(err)?;
        if real > 0 {
            *audio_end = *written + real as u64;
        }
        *written += frames as u64;
        *consumed += real as u64;
        shared.consumed.store(*consumed, Ordering::Release);
        Ok(real)
    };

    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return Ok(());
        }

        let flush = shared.flush_req.load(Ordering::Acquire);
        if flush != ack {
            if started {
                client.stop_stream().map_err(err)?;
                started = false;
            }
            client.reset_stream().map_err(err)?;
            renderer.drain();
            written = 0;
            audio_end = 0;
            shared.played.store(consumed, Ordering::Release);
            ack = flush;
            shared.flush_ack.store(ack, Ordering::Release);
            prefill_since = None;
            continue;
        }

        if !shared.running.load(Ordering::Acquire) {
            if started {
                // Stop keeps the buffered audio; Start resumes exactly there.
                client.stop_stream().map_err(err)?;
                started = false;
            }
            prefill_since = None;
            std::thread::park_timeout(Duration::from_millis(250));
            continue;
        }

        if !started {
            // Wait (briefly) for enough audio so playback starts cleanly.
            let since = *prefill_since.get_or_insert_with(Instant::now);
            let ready = renderer.available_frames() >= prefill_frames
                || shared.draining.load(Ordering::Acquire)
                || since.elapsed() > Duration::from_millis(300);
            if !ready {
                std::thread::park_timeout(Duration::from_millis(2));
                continue;
            }
            let space = client.get_available_space_in_frames().map_err(err)? as usize;
            write(renderer, space, &mut written, &mut audio_end, &mut consumed)?;
            client.start_stream().map_err(err)?;
            started = true;
            timeouts = 0;
            continue;
        }

        if event.wait_for_event(200).is_err() {
            timeouts += 1;
            if timeouts > MAX_EVENT_TIMEOUTS {
                return Err("the audio device stopped responding".into());
            }
            continue;
        }
        timeouts = 0;

        let space = client.get_available_space_in_frames().map_err(err)? as usize;
        let before = renderer.available_frames();
        let real = write(renderer, space, &mut written, &mut audio_end, &mut consumed)?;
        if real < space && !shared.draining.load(Ordering::Acquire) && before < space {
            shared.underruns.fetch_add(1, Ordering::Relaxed);
        }

        // Frames the device has actually played: everything except what is
        // still queued in its buffer.
        let padding = u64::from(client.get_current_padding().map_err(err)?);
        let device_pos = written.saturating_sub(padding);
        let unplayed = audio_end.saturating_sub(device_pos);
        shared
            .played
            .store(consumed.saturating_sub(unplayed), Ordering::Release);
    }
}

fn to_wave(f: &StreamFormat) -> WaveFormat {
    let (store, valid, kind) = match f.sample {
        SampleFormat::Int { container, valid } => (container, valid, SampleType::Int),
        SampleFormat::Float32 => (32, 32, SampleType::Float),
    };
    WaveFormat::new(
        usize::from(store),
        usize::from(valid),
        kind,
        f.rate as usize,
        usize::from(f.channels),
        None,
    )
}

fn from_wave(w: &WaveFormat) -> Option<StreamFormat> {
    let sample = match w.get_subformat().ok()? {
        SampleType::Float if w.get_bitspersample() == 32 => SampleFormat::Float32,
        SampleType::Float => return None,
        SampleType::Int => SampleFormat::Int {
            container: w.get_bitspersample(),
            valid: w.get_validbitspersample(),
        },
    };
    Some(StreamFormat {
        rate: w.get_samplespersec(),
        channels: w.get_nchannels(),
        sample,
    })
}

fn describe_exclusive_error(e: &WasapiError) -> String {
    if let WasapiError::Windows(we) = e {
        let code = we.code();
        if code == AUDCLNT_E_DEVICE_IN_USE {
            return "another app is using the device in exclusive mode".into();
        }
        if code == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED {
            return "exclusive mode is disabled for this device in Windows sound settings".into();
        }
        if code == AUDCLNT_E_DEVICE_INVALIDATED {
            return "the device was removed or reconfigured".into();
        }
    }
    format!("exclusive mode failed: {e}")
}

fn err(e: WasapiError) -> String {
    if let WasapiError::Windows(we) = &e
        && we.code() == AUDCLNT_E_DEVICE_INVALIDATED
    {
        return "the audio device was removed or reconfigured".into();
    }
    e.to_string()
}
