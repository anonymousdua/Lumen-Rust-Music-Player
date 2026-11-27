//! File decoding with symphonia into interleaved `f64` samples.
//!
//! Gapless trimming (MP3 LAME/iTunSMPB delay and padding, Vorbis) is done by the
//! symphonia decoders themselves; FLAC, ALAC and PCM are frame-exact.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag, Tag};
use symphonia::core::units::{Time, TimeBase};

use crate::dsp::{ChannelMap, parse_gain_db, parse_peak};
use crate::protocol::{ReplayGainTags, TrackInfo};

/// Codecs whose output has no meaningful bit depth.
const LOSSY_CODECS: &[&str] = &["mp1", "mp2", "mp3", "vorbis", "aac", "opus"];

/// More consecutive undecodable packets than this and the file is given up.
const MAX_CONSECUTIVE_ERRORS: u32 = 64;

pub struct Decoder {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    info: TrackInfo,
    channels: usize,
    /// Frames still to drop after a seek landed before the requested time.
    skip_frames: u64,
    raw: Vec<f64>,
    out: Vec<f64>,
}

impl Decoder {
    /// Opens and probes `path`. Errors are human-readable.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("cannot open file: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let mut reader = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| format!("unsupported or corrupt file: {e}"))?;

        let track = reader
            .default_track(TrackType::Audio)
            .ok_or("no audio track")?;
        let track_id = track.id;
        let time_base = track.time_base;
        let num_frames = track.num_frames;
        let duration_tb = track.duration;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or("no audio codec parameters")?
            .clone();

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported codec: {e}"))?;

        let codec = decoder.codec_info().short_name.to_string();
        let sample_rate = params.sample_rate.ok_or("unknown sample rate")?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|&c| c > 0)
            .ok_or("unknown channel layout")?;
        let lossy = LOSSY_CODECS.contains(&codec.as_str());
        let bits_per_sample = if lossy { None } else { params.bits_per_sample };

        let duration = num_frames
            .map(|f| Duration::from_secs_f64(f as f64 / f64::from(sample_rate)))
            .or_else(|| {
                let tb = time_base?;
                let t = tb.calc_duration(duration_tb?)?;
                time_to_duration(t)
            });

        let replaygain = read_replaygain(reader.as_mut());

        Ok(Self {
            reader,
            decoder,
            track_id,
            time_base,
            info: TrackInfo {
                codec,
                sample_rate,
                channels: channels as u16,
                bits_per_sample,
                duration,
                replaygain,
            },
            channels,
            skip_frames: 0,
            raw: Vec::new(),
            out: Vec::new(),
        })
    }

    pub fn info(&self) -> &TrackInfo {
        &self.info
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.info.sample_rate
    }

    /// The next block of interleaved samples (`channels()` per frame), or
    /// `None` at the end of the file. Corrupt packets are skipped with a warning.
    pub fn next_block(&mut self) -> Result<Option<&[f64]>, String> {
        let mut errors = 0u32;
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None),
                Err(SymError::ResetRequired) => {
                    // A new chained stream; rebuild the decoder with the same params.
                    let params = self.decoder.codec_params().clone();
                    self.decoder = symphonia::default::get_codecs()
                        .make_audio_decoder(&params, &AudioDecoderOptions::default())
                        .map_err(|e| format!("decoder reset failed: {e}"))?;
                    continue;
                }
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(e) => return Err(format!("read error: {e}")),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let decoded_channels = buf.spec().channels().count().max(1);
                    buf.copy_to_vec_interleaved::<f64>(&mut self.raw);
                    if decoded_channels != self.channels {
                        let map = ChannelMap::new(decoded_channels, self.channels);
                        let mut fixed = Vec::with_capacity(self.raw.len());
                        map.apply(&self.raw, &mut fixed);
                        self.raw = fixed;
                    }
                }
                Err(SymError::DecodeError(e)) => {
                    errors += 1;
                    tracing::warn!("skipping undecodable packet: {e}");
                    if errors > MAX_CONSECUTIVE_ERRORS {
                        return Err(format!("too many decode errors: {e}"));
                    }
                    continue;
                }
                Err(SymError::IoError(e)) => {
                    errors += 1;
                    tracing::warn!("i/o error in packet: {e}");
                    if errors > MAX_CONSECUTIVE_ERRORS {
                        return Err(format!("too many read errors: {e}"));
                    }
                    continue;
                }
                Err(e) => return Err(format!("decode error: {e}")),
            }

            let frames = (self.raw.len() / self.channels) as u64;
            if frames == 0 {
                continue;
            }
            let drop = self.skip_frames.min(frames);
            self.skip_frames -= drop;
            if drop == frames {
                continue;
            }
            let start = drop as usize * self.channels;
            std::mem::swap(&mut self.raw, &mut self.out);
            return Ok(Some(&self.out[start..]));
        }
    }

    /// Seeks to `position` (sample-accurate). Returns the position reached.
    pub fn seek(&mut self, position: Duration) -> Result<Duration, String> {
        // Half a frame on top: the reader rounds the time down to a frame, so
        // a position computed from a frame count lands on that frame exactly.
        let secs = position.as_secs_f64() + 0.5 / f64::from(self.info.sample_rate.max(1));
        let time = Time::try_from_secs_f64(secs).unwrap_or(Time::ZERO);
        let seeked = self
            .reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| format!("seek failed: {e}"))?;
        self.decoder.reset();
        let delta = seeked.required_ts.get() - seeked.actual_ts.get();
        self.skip_frames = self.ts_to_frames(delta.max(0));
        let reached = self
            .time_base
            .and_then(|tb| tb.calc_time(seeked.required_ts))
            .and_then(time_to_duration)
            .unwrap_or(position);
        Ok(reached)
    }

    fn ts_to_frames(&self, ts: i64) -> u64 {
        match self.time_base {
            Some(tb) => {
                let secs = ts as f64 * f64::from(tb.numer.get()) / f64::from(tb.denom.get());
                (secs * f64::from(self.info.sample_rate)).round() as u64
            }
            None => ts as u64,
        }
    }
}

fn time_to_duration(t: Time) -> Option<Duration> {
    let secs = t.as_secs_f64();
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

/// Collects ReplayGain values from every metadata revision the reader has.
fn read_replaygain(reader: &mut dyn FormatReader) -> ReplayGainTags {
    let mut tags = ReplayGainTags::default();
    let mut md = reader.metadata();
    loop {
        if let Some(rev) = md.current() {
            apply_revision(rev, &mut tags);
        }
        if md.pop().is_none() {
            break;
        }
    }
    tags
}

fn apply_revision(rev: &MetadataRevision, tags: &mut ReplayGainTags) {
    let all = rev
        .media
        .tags
        .iter()
        .chain(rev.per_track.iter().flat_map(|t| t.metadata.tags.iter()));
    for tag in all {
        apply_tag(tag, tags);
    }
}

fn apply_tag(tag: &Tag, rg: &mut ReplayGainTags) {
    match &tag.std {
        Some(StandardTag::ReplayGainTrackGain(v)) => rg.track_gain_db = parse_gain_db(v),
        Some(StandardTag::ReplayGainTrackPeak(v)) => rg.track_peak = parse_peak(v),
        Some(StandardTag::ReplayGainAlbumGain(v)) => rg.album_gain_db = parse_gain_db(v),
        Some(StandardTag::ReplayGainAlbumPeak(v)) => rg.album_peak = parse_peak(v),
        Some(_) => {}
        None => {
            // Raw keys some taggers use (e.g. ID3 TXXX descriptions).
            let key = tag.raw.key.to_ascii_uppercase();
            let value = tag.raw.value.to_string();
            match key.trim_start_matches("TXXX:") {
                "REPLAYGAIN_TRACK_GAIN" => rg.track_gain_db = parse_gain_db(&value),
                "REPLAYGAIN_TRACK_PEAK" => rg.track_peak = parse_peak(&value),
                "REPLAYGAIN_ALBUM_GAIN" => rg.album_gain_db = parse_gain_db(&value),
                "REPLAYGAIN_ALBUM_PEAK" => rg.album_peak = parse_peak(&value),
                _ => {}
            }
        }
    }
}
