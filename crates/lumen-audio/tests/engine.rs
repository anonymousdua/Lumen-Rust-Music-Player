//! End-to-end engine tests against the in-memory capture device.
//!
//! The capture device negotiates formats like a real one and records exactly
//! the samples the device would receive, so these tests check bit-perfection,
//! gapless joins, seeking and fallback without audio hardware.

mod common;

use std::time::Duration;

use common::*;
use lumen_audio::{Command, Event, OutputMode, PlaybackState, QueueItem, SampleFormat, SignalPath};

fn queue(paths: &[std::path::PathBuf], play: bool) -> Command {
    Command::SetQueue {
        items: paths.iter().map(QueueItem::from_path).collect(),
        start: 0,
        position: Duration::ZERO,
        play,
    }
}

fn signal_paths(events: &[Event]) -> Vec<SignalPath> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::SignalPath(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn track_changes(events: &[Event]) -> Vec<usize> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TrackChanged { index, .. } => Some(*index),
            _ => None,
        })
        .collect()
}

fn assert_exact(captured: &[f64], expected: &[f64]) {
    assert_eq!(captured.len(), expected.len(), "sample count differs");
    if let Some(i) = captured.iter().zip(expected).position(|(a, b)| a != b) {
        panic!(
            "sample {i} differs: got {}, expected {}",
            captured[i], expected[i]
        );
    }
}

#[test]
fn exclusive_24_96_flac_is_bit_perfect() {
    let dir = temp_dir("bitperfect");
    let path = dir.join("hires.flac");
    let samples = signal(96_000 * 3 / 2, 2, 24, 96_000, 7);
    write_flac(&path, &samples, 2, 24, 96_000);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[path], true));
    let events = until_end(&engine);

    let sp = signal_paths(&events);
    assert_eq!(sp.len(), 1);
    assert_eq!(sp[0].mode, OutputMode::Exclusive);
    assert_eq!(sp[0].output.rate, 96_000);
    assert_eq!(
        sp[0].output.sample,
        SampleFormat::Int {
            container: 32,
            valid: 24
        }
    );
    assert_eq!(sp[0].source.bits_per_sample, Some(24));
    assert_eq!(sp[0].source.codec, "flac");
    assert!(sp[0].bit_perfect, "{:?}", sp[0]);
    assert_eq!(sp[0].mode_badge(), "WASAPI Exclusive");

    let streams = dev.streams();
    assert_eq!(streams.len(), 1);
    let expected: Vec<f64> = samples.iter().map(|&s| device_value(s, 24)).collect();
    assert_exact(&streams[0].samples, &expected);
}

#[test]
fn album_plays_gapless_on_one_stream() {
    let dir = temp_dir("gapless");
    let rate = 44_100;
    let all = signal(70_000, 2, 16, rate, 11);
    // Split at odd frame counts, mid-waveform.
    let cuts = [0, 20_000, 53_333, 70_000];
    let mut paths = Vec::new();
    for (n, w) in cuts.windows(2).enumerate() {
        let p = dir.join(format!("{:02}.flac", n + 1));
        write_flac(&p, &all[w[0] * 2..w[1] * 2], 2, 16, rate);
        paths.push(p);
    }

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&paths, true));
    let events = until_end(&engine);

    assert_eq!(track_changes(&events), vec![0, 1, 2]);
    let streams = dev.streams();
    assert_eq!(
        streams.len(),
        1,
        "the device must not be reopened between tracks"
    );
    let expected: Vec<f64> = all.iter().map(|&s| device_value(s, 16)).collect();
    assert_exact(&streams[0].samples, &expected);
    // The engine ends stopped after the queue.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::StateChanged(PlaybackState::Stopped)))
    );
}

#[test]
fn mixed_bit_depths_stay_on_one_wide_stream() {
    let dir = temp_dir("mixed-depth");
    let a = signal(30_000, 2, 24, 44_100, 3);
    let b = signal(30_000, 2, 16, 44_100, 4);
    let (pa, pb) = (dir.join("a.flac"), dir.join("b.flac"));
    write_flac(&pa, &a, 2, 24, 44_100);
    write_flac(&pb, &b, 2, 16, 44_100);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[pa, pb], true));
    let events = until_end(&engine);

    let streams = dev.streams();
    assert_eq!(streams.len(), 1, "16-bit fits the 24-bit stream exactly");
    let mut expected: Vec<f64> = a.iter().map(|&s| device_value(s, 24)).collect();
    expected.extend(b.iter().map(|&s| device_value(s, 16)));
    assert_exact(&streams[0].samples, &expected);
    let sp = signal_paths(&events);
    assert!(sp.iter().all(|s| s.bit_perfect), "{sp:?}");
}

#[test]
fn rate_change_reopens_the_device_at_the_new_rate() {
    let dir = temp_dir("rate-change");
    let a = signal(20_000, 2, 16, 44_100, 5);
    let b = signal(20_000, 2, 16, 48_000, 6);
    let (pa, pb) = (dir.join("a.flac"), dir.join("b.flac"));
    write_flac(&pa, &a, 2, 16, 44_100);
    write_flac(&pb, &b, 2, 16, 48_000);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[pa, pb], true));
    let events = until_end(&engine);

    assert_eq!(track_changes(&events), vec![0, 1]);
    let streams = dev.streams();
    assert_eq!(streams.len(), 2);
    assert_eq!(
        streams[0].info.as_ref().map(|i| i.format.rate),
        Some(44_100)
    );
    assert_eq!(
        streams[1].info.as_ref().map(|i| i.format.rate),
        Some(48_000)
    );
    let exp_b: Vec<f64> = b.iter().map(|&s| device_value(s, 16)).collect();
    assert_exact(&streams[1].samples, &exp_b);
}

#[test]
fn exclusive_refused_falls_back_to_shared_and_resamples() {
    let dir = temp_dir("fallback");
    let path = dir.join("hires.flac");
    let samples = signal(96_000, 2, 24, 96_000, 9); // 1 second
    write_flac(&path, &samples, 2, 24, 96_000);

    // This device only takes 48 kHz in exclusive mode.
    let dev = capture_device(|f| f.rate == 48_000);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[path], true));
    let events = until_end(&engine);

    let sp = signal_paths(&events);
    assert_eq!(sp.len(), 1);
    assert_eq!(sp[0].mode, OutputMode::Shared);
    assert_eq!(sp[0].resampled, Some((96_000, 48_000)));
    assert!(!sp[0].bit_perfect);
    let reason = sp[0].exclusive_fallback.clone().unwrap_or_default();
    assert!(reason.contains("96000 Hz"), "{reason}");
    assert_eq!(sp[0].mode_badge(), "Shared · resampled to 48 kHz");

    let streams = dev.streams();
    assert_eq!(streams.len(), 1);
    let frames = streams[0].samples.len() / 2;
    assert_eq!(frames, 48_000, "resampled length matches the duration");
}

#[test]
fn exclusive_init_failure_falls_back_to_shared() {
    let dir = temp_dir("init-fail");
    let path = dir.join("a.flac");
    write_flac(&path, &signal(48_000, 2, 16, 48_000, 2), 2, 16, 48_000);

    let dev = std::sync::Arc::new(
        lumen_audio::CaptureDevice::new("Busy DAC", MIX_48K, |_| true)
            .with_exclusive_init_error("another app is using the device in exclusive mode"),
    );
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[path], true));
    let events = until_end(&engine);
    let sp = signal_paths(&events);
    assert_eq!(sp[0].mode, OutputMode::Shared);
    assert_eq!(
        sp[0].resampled, None,
        "same rate: no resampling in shared mode"
    );
    assert!(
        sp[0]
            .exclusive_fallback
            .clone()
            .unwrap_or_default()
            .contains("another app")
    );
}

#[test]
fn shared_mode_resampled_album_has_no_gaps() {
    let dir = temp_dir("shared-gapless");
    let rate = 44_100;
    let all = signal(44_100, 2, 16, rate, 13);
    let (pa, pb) = (dir.join("a.flac"), dir.join("b.flac"));
    write_flac(&pa, &all[..22_222 * 2], 2, 16, rate);
    write_flac(&pb, &all[22_222 * 2..], 2, 16, rate);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, false, 1.0);
    engine.send(queue(&[pa, pb], true));
    let events = until_end(&engine);
    assert_eq!(track_changes(&events), vec![0, 1]);

    let streams = dev.streams();
    assert_eq!(streams.len(), 1);
    let out = &streams[0].samples;
    assert_eq!(out.len() / 2, 48_000);
    // The signal is a sine plus faint noise: neighbouring samples never jump.
    // A gap or a dropped block would show as a large step.
    let max_step = out
        .as_chunks::<2>()
        .0
        .iter()
        .map(|f| f[0])
        .collect::<Vec<_>>()
        .windows(2)
        .skip(200)
        .take(47_500)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f64, f64::max);
    assert!(max_step < 0.05, "discontinuity of {max_step}");
}

#[test]
fn start_position_seeks_sample_accurately() {
    let dir = temp_dir("seek-start");
    let path = dir.join("a.flac");
    let samples = signal(44_100 * 3, 2, 16, 44_100, 21);
    write_flac(&path, &samples, 2, 16, 44_100);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(Command::SetQueue {
        items: vec![QueueItem::from_path(&path)],
        start: 0,
        position: Duration::from_millis(1_500),
        play: true,
    });
    until_end(&engine);
    let streams = dev.streams();
    let expected: Vec<f64> = samples[66_150 * 2..]
        .iter()
        .map(|&s| device_value(s, 16))
        .collect();
    assert_exact(&streams[0].samples, &expected);
}

#[test]
fn seek_while_playing_flushes_and_continues_exactly() {
    let dir = temp_dir("seek-live");
    let path = dir.join("a.flac");
    let samples = signal(44_100 * 8, 2, 16, 44_100, 23);
    write_flac(&path, &samples, 2, 16, 44_100);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[path], true));
    engine.send(Command::Seek(Duration::from_secs(5)));
    let events = until_end(&engine);

    let streams = dev.streams();
    assert_eq!(streams.len(), 1, "seeking keeps the device open");
    let s = &streams[0];
    let flush = *s
        .flushes
        .last()
        .unwrap_or_else(|| panic!("no flush recorded"));
    let expected: Vec<f64> = samples[5 * 44_100 * 2..]
        .iter()
        .map(|&v| device_value(v, 16))
        .collect();
    assert_exact(&s.samples[flush..], &expected);
    // Position after the seek starts at 5 s.
    let after_seek = events.iter().find_map(|e| match e {
        Event::Position { position, .. } if *position >= Duration::from_secs(5) => Some(*position),
        _ => None,
    });
    assert!(after_seek.is_some());
}

#[test]
fn paused_seek_then_play() {
    let dir = temp_dir("seek-paused");
    let path = dir.join("a.flac");
    let samples = signal(44_100 * 3, 2, 16, 44_100, 25);
    write_flac(&path, &samples, 2, 16, 44_100);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    // Resume-on-launch style: loaded paused, nothing opened yet.
    engine.send(queue(&[path], false));
    engine.send(Command::Seek(Duration::from_secs(2)));
    let events = events_until(
        &engine,
        Duration::from_secs(5),
        |e| matches!(e, Event::Position { position, .. } if *position == Duration::from_secs(2)),
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::StateChanged(PlaybackState::Paused)))
    );
    assert!(
        dev.streams().is_empty(),
        "paused load must not open the device"
    );

    engine.send(Command::Play);
    until_end(&engine);
    let streams = dev.streams();
    let expected: Vec<f64> = samples[2 * 44_100 * 2..]
        .iter()
        .map(|&v| device_value(v, 16))
        .collect();
    assert_exact(&streams[0].samples, &expected);
}

#[test]
fn pause_and_resume_lose_nothing() {
    let dir = temp_dir("pause");
    let path = dir.join("a.flac");
    let samples = signal(44_100 * 2, 2, 16, 44_100, 27);
    write_flac(&path, &samples, 2, 16, 44_100);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[path], true));
    engine.send(Command::Pause);
    events_until(&engine, Duration::from_secs(5), |e| {
        matches!(e, Event::StateChanged(PlaybackState::Paused))
    });
    std::thread::sleep(Duration::from_millis(50));
    // Paused in exclusive mode: the device is free for other programs.
    assert!(
        dev.streams().iter().all(|s| s.closed),
        "pause must release an exclusive device"
    );
    engine.send(Command::Play);
    until_end(&engine);
    // Resuming reopens the file at the paused frame: nothing lost or repeated.
    let got: Vec<f64> = dev
        .streams()
        .iter()
        .flat_map(|s| s.samples.iter().copied())
        .collect();
    let expected: Vec<f64> = samples.iter().map(|&v| device_value(v, 16)).collect();
    assert_exact(&got, &expected);
}

#[test]
fn shared_pause_keeps_the_stream() {
    let dir = temp_dir("pause-shared");
    let path = dir.join("a.flac");
    let samples = signal(44_100 * 2, 2, 16, 44_100, 28);
    write_flac(&path, &samples, 2, 16, 44_100);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, false, 1.0);
    engine.send(queue(&[path], true));
    engine.send(Command::Pause);
    events_until(&engine, Duration::from_secs(5), |e| {
        matches!(e, Event::StateChanged(PlaybackState::Paused))
    });
    std::thread::sleep(Duration::from_millis(50));
    // Shared mode doesn't block anyone, so the stream stays for an instant resume.
    let streams = dev.streams();
    assert_eq!(streams.len(), 1);
    assert!(!streams[0].closed);
    engine.send(Command::Play);
    until_end(&engine);
    assert_eq!(dev.streams().len(), 1);
}

#[test]
fn missing_file_is_skipped_with_an_event() {
    let dir = temp_dir("missing");
    let good = dir.join("good.flac");
    let samples = signal(10_000, 2, 16, 44_100, 29);
    write_flac(&good, &samples, 2, 16, 44_100);
    let missing = dir.join("gone.flac");

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[missing.clone(), good], true));
    let events = until_end(&engine);
    let skipped = events.iter().find_map(|e| match e {
        Event::TrackSkipped {
            index,
            path,
            reason,
        } => Some((*index, path.clone(), reason.clone())),
        _ => None,
    });
    assert_eq!(skipped.map(|s| (s.0, s.1)), Some((0, missing)));
    assert_eq!(track_changes(&events), vec![1]);
    assert_eq!(dev.streams()[0].samples.len(), samples.len());
}

#[test]
fn corrupt_file_mid_album_is_skipped_gaplessly() {
    let dir = temp_dir("corrupt");
    let a = dir.join("a.flac");
    let bad = dir.join("b.flac");
    let c = dir.join("c.flac");
    let sa = signal(10_000, 2, 16, 44_100, 31);
    let sc = signal(10_000, 2, 16, 44_100, 33);
    write_flac(&a, &sa, 2, 16, 44_100);
    std::fs::write(&bad, b"this is not audio at all").expect("write");
    write_flac(&c, &sc, 2, 16, 44_100);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[a, bad, c], true));
    let events = until_end(&engine);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::TrackSkipped { index: 1, .. }))
    );
    assert_eq!(track_changes(&events), vec![0, 2]);
    assert_eq!(dev.streams()[0].samples.len(), sa.len() + sc.len());
}

#[test]
fn volume_below_100_percent_is_reported_and_applied() {
    let dir = temp_dir("volume");
    let path = dir.join("a.flac");
    let samples = signal(20_000, 2, 24, 48_000, 35);
    write_flac(&path, &samples, 2, 24, 48_000);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 0.5);
    engine.send(queue(&[path], true));
    let events = until_end(&engine);
    let sp = signal_paths(&events);
    assert!(sp[0].volume_attenuates);
    assert!(!sp[0].bit_perfect);
    let out = &dev.streams()[0].samples;
    // Skip the first 5 ms ramp-in check: gain starts at 0.5 already.
    for (got, s) in out.iter().zip(&samples) {
        let want = device_value(*s, 24) * 0.5;
        assert!((got - want).abs() <= 256.0, "got {got}, want {want}");
    }
}

#[test]
fn replaygain_track_gain_is_applied_and_reported() {
    let dir = temp_dir("rg");
    let path = dir.join("a.flac");
    let samples = signal(20_000, 2, 24, 48_000, 37);
    write_flac_with_tags(
        &path,
        &samples,
        2,
        24,
        48_000,
        &[
            ("REPLAYGAIN_TRACK_GAIN", "-6.02 dB"),
            ("REPLAYGAIN_TRACK_PEAK", "0.300000"),
        ],
    );

    let dev = capture_device(|_| true);
    let engine = lumen_audio::Engine::start(lumen_audio::EngineConfig {
        backend: lumen_audio::Backend::Capture(dev.clone()),
        replaygain: lumen_core::ReplayGainMode::Track,
        ..Default::default()
    })
    .expect("engine");
    engine.send(queue(&[path], true));
    let events = until_end(&engine);
    let sp = signal_paths(&events);
    assert_eq!(sp[0].source.replaygain.track_gain_db, Some(-6.02));
    assert_eq!(sp[0].replaygain_db, Some(-6.02));
    assert!(!sp[0].bit_perfect, "ReplayGain changes samples");
    let out = &dev.streams()[0].samples;
    let factor = 10f64.powf(-6.02 / 20.0);
    for (got, s) in out.iter().zip(&samples).skip(100) {
        let want = device_value(*s, 24) * factor;
        assert!((got - want).abs() <= 256.0, "got {got}, want {want}");
    }
}

#[test]
fn wav_and_mono_sources() {
    let dir = temp_dir("wav");
    let w24 = dir.join("a24.wav");
    let s24 = signal(10_000, 2, 24, 48_000, 41);
    write_wav(&w24, &s24, 2, 24, 48_000);
    let mono = dir.join("mono.wav");
    let sm = signal(10_000, 1, 16, 48_000, 43);
    write_wav(&mono, &sm, 1, 16, 48_000);

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[w24, mono], true));
    let events = until_end(&engine);
    let sp = signal_paths(&events);
    assert_eq!(sp.len(), 2);
    assert!(sp.iter().all(|s| s.bit_perfect), "{sp:?}");

    let streams = dev.streams();
    assert_eq!(
        streams.len(),
        1,
        "mono 16-bit fits the 24-bit stereo stream"
    );
    let mut expected: Vec<f64> = s24.iter().map(|&s| device_value(s, 24)).collect();
    for &m in &sm {
        let v = device_value(m, 16);
        expected.push(v);
        expected.push(v);
    }
    assert_exact(&streams[0].samples, &expected);
}

#[test]
fn float_wav_is_exact_on_a_float_device() {
    let dir = temp_dir("f32");
    let path = dir.join("f.wav");
    let samples: Vec<f32> = signal(5_000, 2, 24, 44_100, 45)
        .iter()
        .map(|&s| s as f32 / 8_388_608.0 * 0.9)
        .collect();
    write_wav_f32(&path, &samples, 2, 44_100);

    let dev = capture_device(|f| f.sample == SampleFormat::Float32);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[path], true));
    until_end(&engine);
    let expected: Vec<f64> = samples.iter().map(|&s| f64::from(s)).collect();
    assert_exact(&dev.streams()[0].samples, &expected);
}

#[test]
fn next_prev_and_play_index() {
    let dir = temp_dir("nav");
    let paths: Vec<_> = (0..3)
        .map(|i| {
            let p = dir.join(format!("{i}.flac"));
            write_flac(
                &p,
                &signal(44_100 * 20, 2, 16, 44_100, 50 + i),
                2,
                16,
                44_100,
            );
            p
        })
        .collect();
    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&paths, false));
    engine.send(Command::Next);
    engine.send(Command::Next);
    engine.send(Command::Prev);
    engine.send(Command::PlayIndex(0));
    let events = events_until(&engine, Duration::from_secs(5), |e| {
        matches!(e, Event::StateChanged(PlaybackState::Playing))
    });
    assert_eq!(track_changes(&events), vec![0, 1, 2, 1, 0]);
    engine.send(Command::Stop);
    events_until(&engine, Duration::from_secs(5), |e| {
        matches!(e, Event::StateChanged(PlaybackState::Stopped))
    });
}

#[test]
fn devices_are_listed() {
    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(Command::ListDevices);
    let events = events_until(&engine, Duration::from_secs(5), |e| {
        matches!(e, Event::Devices(_))
    });
    match events.last() {
        Some(Event::Devices(d)) => assert_eq!(d[0].name, "Test DAC"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn after_a_refused_rate_the_next_rate_retries_exclusive() {
    let dir = temp_dir("retry-exclusive");
    let a = signal(10_000, 2, 16, 44_100, 61);
    let b = signal(10_000, 2, 16, 48_000, 62);
    let (pa, pb) = (dir.join("a.flac"), dir.join("b.flac"));
    write_flac(&pa, &a, 2, 16, 44_100);
    write_flac(&pb, &b, 2, 16, 48_000);

    // Like an HDMI sink: no 44.1 kHz in exclusive mode.
    let dev = capture_device(|f| f.rate == 48_000);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(queue(&[pa, pb], true));
    let events = until_end(&engine);
    let sp = signal_paths(&events);
    assert_eq!(sp.len(), 2);
    assert_eq!(sp[0].mode, OutputMode::Shared);
    assert_eq!(sp[1].mode, OutputMode::Exclusive);
    assert!(sp[1].bit_perfect);
    let streams = dev.streams();
    assert_eq!(streams.len(), 2);
    let exp_b: Vec<f64> = b.iter().map(|&s| device_value(s, 16)).collect();
    assert_exact(&streams[1].samples, &exp_b);
}

#[test]
fn play_next_and_add_to_queue_play_gaplessly_in_order() {
    let dir = temp_dir("enqueue");
    let parts: Vec<Vec<i32>> = (0..4)
        .map(|i| signal(15_000 + 777 * i, 2, 16, 44_100, 70 + i as u32))
        .collect();
    let paths: Vec<_> = parts
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let p = dir.join(format!("{i}.flac"));
            write_flac(&p, s, 2, 16, 44_100);
            p
        })
        .collect();

    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    // Queue a, b; then "Play next" c and "Add to queue" d.
    engine.send(queue(&paths[..2], false));
    engine.send(Command::Enqueue {
        items: vec![QueueItem::from_path(&paths[2])],
        next: true,
    });
    engine.send(Command::Enqueue {
        items: vec![QueueItem::from_path(&paths[3])],
        next: false,
    });
    engine.send(Command::Play);
    let events = until_end(&engine);

    // Item indices: a=0, b=1, c=2, d=3. Play order a, c, b, d.
    assert_eq!(track_changes(&events), vec![0, 2, 1, 3]);
    let streams = dev.streams();
    assert_eq!(streams.len(), 1);
    let expected: Vec<f64> = [0, 2, 1, 3]
        .iter()
        .flat_map(|&i| parts[i].iter().map(|&s| device_value(s, 16)))
        .collect();
    assert_exact(&streams[0].samples, &expected);
}

#[test]
fn enqueue_into_an_empty_queue_loads_paused() {
    let dir = temp_dir("enqueue-empty");
    let p = dir.join("a.flac");
    write_flac(&p, &signal(10_000, 2, 16, 44_100, 80), 2, 16, 44_100);
    let dev = capture_device(|_| true);
    let engine = engine_with(&dev, true, 1.0);
    engine.send(Command::Enqueue {
        items: vec![QueueItem::from_path(&p)],
        next: false,
    });
    let events = events_until(&engine, Duration::from_secs(5), |e| {
        matches!(e, Event::StateChanged(PlaybackState::Paused))
    });
    assert_eq!(track_changes(&events), vec![0]);
    assert!(dev.streams().is_empty(), "no device opened while paused");
}

#[test]
fn fixed_output_rate_resamples_in_exclusive_mode_and_says_so() {
    let dir = temp_dir("fixed-rate");
    let p = dir.join("a.flac");
    write_flac(&p, &signal(44_100, 2, 16, 44_100, 90), 2, 16, 44_100);
    let dev = capture_device(|_| true);
    let engine = lumen_audio::Engine::start(lumen_audio::EngineConfig {
        backend: lumen_audio::Backend::Capture(dev.clone()),
        output: lumen_audio::OutputConfig {
            device_id: None,
            exclusive: true,
            rate: Some(48_000),
        },
        ..Default::default()
    })
    .expect("engine");
    engine.send(queue(&[p], true));
    let events = until_end(&engine);
    let sp = signal_paths(&events);
    assert_eq!(sp[0].mode, OutputMode::Exclusive);
    assert_eq!(sp[0].output.rate, 48_000);
    assert_eq!(sp[0].resampled, Some((44_100, 48_000)));
    assert!(!sp[0].bit_perfect);
    assert_eq!(sp[0].mode_badge(), "WASAPI Exclusive · resampled to 48 kHz");
    // One second of audio arrives as about one second at the new rate.
    let frames = dev.streams()[0].samples.len() / 2;
    assert!((47_000..=49_000).contains(&frames), "{frames} frames");
}
