//! Reading one audio file: tags and properties (lofty), lyrics presence,
//! cover art and the thumbnail cache.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use lofty::file::FileType;
use lofty::picture::PictureType;
use lofty::prelude::*;
use lumen_core::LyricsFlags;

/// Size and modification time, the incremental-rescan key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    pub mtime_ms: i64,
}

pub fn stat(path: &Path) -> std::io::Result<FileStat> {
    let md = fs::metadata(path)?;
    let mtime_ms = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok(FileStat {
        size: md.len(),
        mtime_ms,
    })
}

/// Everything the scanner learns from one file.
#[derive(Clone, Debug, PartialEq)]
pub struct ScannedTrack {
    pub path: PathBuf,
    pub stat: FileStat,
    pub title: String,
    pub artist: String,
    pub album_artist: Option<String>,
    pub album: String,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub duration_ms: u64,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub bitrate: Option<u32>,
    pub codec: String,
    pub cover_hash: Option<String>,
    pub lyrics: LyricsFlags,
    pub lrc_mtime: Option<i64>,
}

pub const UNKNOWN_ARTIST: &str = "Unknown Artist";
pub const UNKNOWN_ALBUM: &str = "Unknown Album";

/// Reads tags, properties, lyrics flags and cover of `path`.
pub fn read_track(
    path: &Path,
    stat: FileStat,
    thumbs: &ThumbCache,
) -> Result<ScannedTrack, String> {
    let tagged = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    let props = tagged.properties();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let text = |s: Option<std::borrow::Cow<'_, str>>| -> Option<String> {
        s.map(|v| v.trim().trim_matches('\0').to_string())
            .filter(|v| !v.is_empty())
    };
    let get = |key: ItemKey| -> Option<String> {
        tag.and_then(|t| t.get_string(key))
            .map(|v| v.trim().trim_matches('\0').to_string())
            .filter(|v| !v.is_empty())
    };

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = tag.and_then(|t| text(t.title())).unwrap_or(stem);
    let artist = tag
        .and_then(|t| text(t.artist()))
        .or_else(|| get(ItemKey::AlbumArtist))
        .unwrap_or_else(|| UNKNOWN_ARTIST.to_string());
    let album = tag
        .and_then(|t| text(t.album()))
        .unwrap_or_else(|| UNKNOWN_ALBUM.to_string());
    let year = tag
        .and_then(|t| t.date())
        .map(|d| i32::from(d.year))
        .or_else(|| get(ItemKey::Year).and_then(|y| parse_year(&y)))
        .or_else(|| get(ItemKey::RecordingDate).and_then(|y| parse_year(&y)))
        .filter(|y| *y > 0);

    let bit_depth = props.bit_depth();
    let codec = match tagged.file_type() {
        FileType::Flac => "FLAC",
        FileType::Mpeg => "MP3",
        FileType::Mp4 if bit_depth.is_some() => "ALAC",
        FileType::Mp4 => "AAC",
        FileType::Wav => "WAV",
        FileType::Vorbis => "Vorbis",
        FileType::Opus => "Opus",
        FileType::Aiff => "AIFF",
        FileType::Aac => "AAC",
        FileType::Ape => "APE",
        FileType::WavPack => "WavPack",
        _ => "Audio",
    }
    .to_string();

    // Embedded lyrics.
    let mut lyrics = 0u8;
    if let Some(l) = get(ItemKey::Lyrics).or_else(|| get(ItemKey::UnsyncLyrics)) {
        lyrics |= if has_line_timestamps(&l) {
            LyricsFlags::EMBEDDED_SYNCED
        } else {
            LyricsFlags::EMBEDDED
        };
    }
    if tagged.file_type() == FileType::Mpeg && id3_has_frame(path, b"SYLT") {
        lyrics |= LyricsFlags::EMBEDDED_SYNCED;
    }
    let (lrc_flags, lrc_mtime) = lrc_flags(path);
    lyrics |= lrc_flags.0;

    // Cover: embedded front cover, else any picture, else folder art.
    let cover_hash = tag
        .and_then(|t| {
            t.get_picture_type(PictureType::CoverFront)
                .or_else(|| t.pictures().first())
        })
        .and_then(|p| thumbs.store(p.data()))
        .or_else(|| path.parent().and_then(|d| thumbs.folder_art(d)));

    Ok(ScannedTrack {
        path: path.to_path_buf(),
        stat,
        title,
        artist,
        album_artist: get(ItemKey::AlbumArtist),
        album,
        track_no: tag.and_then(|t| t.track()).filter(|n| *n > 0),
        disc_no: tag.and_then(|t| t.disk()).filter(|n| *n > 0),
        year,
        genre: tag.and_then(|t| text(t.genre())),
        duration_ms: props.duration().as_millis() as u64,
        sample_rate: props.sample_rate(),
        bit_depth,
        channels: props.channels(),
        bitrate: props.audio_bitrate(),
        codec,
        cover_hash,
        lyrics: LyricsFlags(lyrics),
        lrc_mtime,
    })
}

fn parse_year(s: &str) -> Option<i32> {
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .take(4)
        .collect();
    (digits.len() == 4).then(|| digits.parse().ok()).flatten()
}

/// The `.lrc` next to `audio` (same file name), if any.
pub fn find_lrc(audio: &Path) -> Option<PathBuf> {
    let lrc = audio.with_extension("lrc");
    lrc.is_file().then_some(lrc)
}

/// LRC-related flags (`LRC`, `LRC_SYNCED`, `LRC_ENHANCED`) and the `.lrc` mtime.
pub fn lrc_flags(audio: &Path) -> (LyricsFlags, Option<i64>) {
    let Some(lrc) = find_lrc(audio) else {
        return (LyricsFlags(0), None);
    };
    let mtime = stat(&lrc).ok().map(|s| s.mtime_ms);
    let mut flags = LyricsFlags::LRC;
    if let Ok(mut f) = fs::File::open(&lrc) {
        let mut buf = Vec::with_capacity(16 * 1024);
        let _ = f.by_ref().take(256 * 1024).read_to_end(&mut buf);
        let text = decode_text(&buf);
        if has_line_timestamps(&text) {
            flags |= LyricsFlags::LRC_SYNCED;
        }
        if has_word_timestamps(&text) {
            flags |= LyricsFlags::LRC_ENHANCED;
        }
    }
    (LyricsFlags(flags), mtime)
}

/// Good-enough decoding for detection (the lyrics crate does it properly).
fn decode_text(buf: &[u8]) -> String {
    let utf16 = |le: bool, b: &[u8]| -> String {
        let units: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                if le {
                    u16::from_le_bytes(*c)
                } else {
                    u16::from_be_bytes(*c)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match buf {
        [0xFF, 0xFE, rest @ ..] => utf16(true, rest),
        [0xFE, 0xFF, rest @ ..] => utf16(false, rest),
        _ => String::from_utf8_lossy(buf).into_owned(),
    }
}

/// True if any line starts with an LRC time tag like `[01:23.45]`.
pub fn has_line_timestamps(text: &str) -> bool {
    text.lines().any(|line| {
        let mut rest = line.trim_start();
        // Several tags may precede the text: [00:12.00][01:30.00]Chorus
        while let Some(r) = rest.strip_prefix('[') {
            if let Some(end) = r.find(']') {
                if is_time(&r[..end]) {
                    return true;
                }
                rest = &r[end + 1..];
            } else {
                break;
            }
        }
        false
    })
}

fn has_word_timestamps(text: &str) -> bool {
    text.match_indices('<').any(|(i, _)| {
        text[i + 1..]
            .find('>')
            .is_some_and(|end| is_time(&text[i + 1..i + 1 + end]))
    })
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss:xx`.
fn is_time(s: &str) -> bool {
    let Some((m, rest)) = s.split_once(':') else {
        return false;
    };
    let sec = rest.split(['.', ':']).next().unwrap_or("");
    !m.is_empty()
        && m.len() <= 3
        && m.chars().all(|c| c.is_ascii_digit())
        && sec.len() == 2
        && sec.chars().all(|c| c.is_ascii_digit())
}

/// Scans the ID3v2 tag at the start of `path` for a frame id (lofty does not
/// surface `SYLT` through its generic tag).
pub fn id3_has_frame(path: &Path, frame: &[u8; 4]) -> bool {
    let Ok(mut f) = fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 10];
    if f.read_exact(&mut header).is_err() || &header[..3] != b"ID3" {
        return false;
    }
    let version = header[3];
    if !(3..=4).contains(&version) {
        return false;
    }
    let size = synchsafe(&header[6..10]) as usize;
    if size > 32 * 1024 * 1024 {
        return false;
    }
    let mut tag = vec![0u8; size];
    if f.read_exact(&mut tag).is_err() {
        return false;
    }
    let mut pos = 0;
    if header[5] & 0x40 != 0 && tag.len() >= 4 {
        // Extended header.
        let ext = if version == 4 {
            synchsafe(&tag[..4]) as usize
        } else {
            u32::from_be_bytes([tag[0], tag[1], tag[2], tag[3]]) as usize + 4
        };
        pos = ext;
    }
    while pos + 10 <= tag.len() {
        let id = &tag[pos..pos + 4];
        if id[0] == 0 {
            break; // padding
        }
        let len = if version == 4 {
            synchsafe(&tag[pos + 4..pos + 8])
        } else {
            u32::from_be_bytes([tag[pos + 4], tag[pos + 5], tag[pos + 6], tag[pos + 7]])
        } as usize;
        if id == frame {
            return true;
        }
        pos += 10 + len;
    }
    false
}

fn synchsafe(b: &[u8]) -> u32 {
    b.iter()
        .take(4)
        .fold(0, |acc, &x| (acc << 7) | u32::from(x & 0x7F))
}

/// Cover thumbnails in `%LOCALAPPDATA%\Lumen\thumbs`, named by a hash of the
/// original image bytes so every album cover is converted once.
pub struct ThumbCache {
    dir: PathBuf,
    done: Mutex<HashSet<u64>>,
    folder_art: Mutex<HashMap<PathBuf, (i64, Option<String>)>>,
}

/// Longest side of a thumbnail in pixels (album grid cards at 2x scale).
pub const THUMB_SIZE: u32 = 320;
const FOLDER_ART: &[&str] = &[
    "cover.jpg",
    "folder.jpg",
    "front.jpg",
    "album.jpg",
    "cover.png",
    "folder.png",
    "front.png",
];

impl ThumbCache {
    pub fn new(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        Self {
            dir,
            done: Mutex::new(HashSet::new()),
            folder_art: Mutex::new(HashMap::new()),
        }
    }

    pub fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.jpg"))
    }

    /// Hashes `image`, writes its thumbnail if missing, returns the hash.
    /// `None` if the bytes are not a decodable image.
    pub fn store(&self, image: &[u8]) -> Option<String> {
        if image.len() < 16 {
            return None;
        }
        let h = fnv1a(image);
        let hash = format!("{h:016x}");
        let first = self.done.lock().map(|mut d| d.insert(h)).unwrap_or(true);
        let target = self.path(&hash);
        if !first || target.exists() {
            return Some(hash);
        }
        match make_thumbnail(image, &target) {
            Ok(()) => Some(hash),
            Err(e) => {
                tracing::warn!("cover art not decodable: {e}");
                if let Ok(mut d) = self.done.lock() {
                    d.remove(&h);
                }
                None
            }
        }
    }

    /// `cover.jpg`/`folder.jpg`/… in `dir`, cached per directory.
    pub fn folder_art(&self, dir: &Path) -> Option<String> {
        let found = FOLDER_ART
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())?;
        let mtime = stat(&found).map(|s| s.mtime_ms).unwrap_or(0);
        if let Ok(cache) = self.folder_art.lock()
            && let Some((m, hash)) = cache.get(dir)
            && *m == mtime
        {
            return hash.clone();
        }
        let hash = fs::read(&found).ok().and_then(|b| self.store(&b));
        if let Ok(mut cache) = self.folder_art.lock() {
            cache.insert(dir.to_path_buf(), (mtime, hash.clone()));
        }
        hash
    }
}

fn make_thumbnail(image: &[u8], target: &Path) -> Result<(), String> {
    let img = image::load_from_memory(image).map_err(|e| e.to_string())?;
    let thumb = img.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgb8();
    let tmp = target.with_extension(format!("tmp{}", std::process::id()));
    {
        let file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut w = std::io::BufWriter::new(file);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, 85)
            .encode_image(&thumb)
            .map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

/// FNV-1a 64-bit (stable across runs and platforms).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_timestamp_detection() {
        assert!(has_line_timestamps("[ar:Someone]\n[00:12.34]Hello"));
        assert!(has_line_timestamps("[00:12.00][01:30.00]Chorus"));
        assert!(has_line_timestamps("[1:02.5]x"));
        assert!(!has_line_timestamps("[ar:Someone]\nJust text\n[Chorus]"));
        assert!(has_word_timestamps("[00:01.00]<00:01.00>Hel <00:01.50>lo"));
        assert!(!has_word_timestamps("[00:01.00]a <b> c"));
    }

    #[test]
    fn years() {
        assert_eq!(parse_year("2024-05-01"), Some(2024));
        assert_eq!(parse_year("1999"), Some(1999));
        assert_eq!(parse_year("99"), None);
    }

    #[test]
    fn utf16_lrc_is_decoded() {
        let mut b = vec![0xFF, 0xFE];
        for u in "[00:01.00]नमस्ते".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        assert!(has_line_timestamps(&decode_text(&b)));
    }

    #[test]
    fn id3_sylt_scan() {
        let dir = std::env::temp_dir().join(format!("lumen-id3-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let p = dir.join("t.mp3");
        // ID3v2.3 tag: TIT2 frame then SYLT frame.
        let mut frames = Vec::new();
        for (id, body) in [
            (b"TIT2", &b"\x00Hi"[..]),
            (b"SYLT", &b"\x00eng\x02\x01x\x00\x00\x00\x00\x10"[..]),
        ] {
            frames.extend_from_slice(id);
            frames.extend_from_slice(&(body.len() as u32).to_be_bytes());
            frames.extend_from_slice(&[0, 0]);
            frames.extend_from_slice(body);
        }
        frames.extend_from_slice(&[0u8; 16]);
        let n = frames.len() as u32;
        let size = [
            (n >> 21 & 0x7F) as u8,
            (n >> 14 & 0x7F) as u8,
            (n >> 7 & 0x7F) as u8,
            (n & 0x7F) as u8,
        ];
        let mut file = b"ID3\x03\x00\x00".to_vec();
        file.extend_from_slice(&size);
        file.extend_from_slice(&frames);
        fs::write(&p, &file).unwrap_or_default();
        assert!(id3_has_frame(&p, b"SYLT"));
        assert!(id3_has_frame(&p, b"TIT2"));
        assert!(!id3_has_frame(&p, b"USLT"));
        let _ = fs::remove_dir_all(&dir);
    }
}
