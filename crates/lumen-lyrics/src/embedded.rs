//! Lyrics stored inside audio files: ID3v2 `SYLT` (synchronised), and the
//! text lyrics tags lofty maps to `ItemKey::Lyrics` (ID3 `USLT`, Vorbis
//! `LYRICS`/`UNSYNCEDLYRICS`, MP4 `©lyr`). Text lyrics that contain LRC
//! timestamps are treated as synced.

use std::io::Read;
use std::path::Path;

use lofty::prelude::*;
use lofty::tag::ItemKey;

use crate::{Line, Lyrics, Source, Word, parse_lrc};

/// Reads embedded lyrics: `SYLT` first, then text lyrics tags.
pub fn read_embedded(path: &Path) -> Option<Lyrics> {
    if let Some(payload) = id3_frame(path, b"SYLT")
        && let Some(mut l) = parse_sylt(&payload)
        && !l.is_empty()
    {
        l.source = Some(Source::EmbeddedSynced);
        return Some(l);
    }
    let file = lofty::read_from_path(path).ok()?;
    let text = file.tags().iter().find_map(|t| {
        t.get_string(ItemKey::Lyrics)
            .or_else(|| t.get_string(ItemKey::UnsyncLyrics))
            .map(str::to_owned)
    })?;
    let mut l = parse_lrc(&text);
    l.source = Some(Source::EmbeddedText);
    Some(l)
}

/// The payload of the first ID3v2.3/2.4 frame `id` at the start of the file.
fn id3_frame(path: &Path, id: &[u8; 4]) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 10];
    f.read_exact(&mut header).ok()?;
    if &header[..3] != b"ID3" || !(3..=4).contains(&header[3]) {
        return None;
    }
    let version = header[3];
    let syncsafe = |b: &[u8]| -> usize {
        b.iter()
            .fold(0usize, |acc, &x| (acc << 7) | usize::from(x & 0x7f))
    };
    let size = syncsafe(&header[6..10]);
    let mut tag = vec![0u8; size.min(64 * 1024 * 1024)];
    f.read_exact(&mut tag).ok()?;
    let mut pos = 0;
    // Skip an extended header.
    if header[5] & 0x40 != 0 && tag.len() >= 4 {
        let ext = if version == 4 {
            syncsafe(&tag[..4])
        } else {
            u32::from_be_bytes([tag[0], tag[1], tag[2], tag[3]]) as usize + 4
        };
        pos = ext;
    }
    while pos + 10 <= tag.len() {
        let fid = &tag[pos..pos + 4];
        if fid[0] == 0 {
            break; // padding
        }
        let len = if version == 4 {
            syncsafe(&tag[pos + 4..pos + 8])
        } else {
            u32::from_be_bytes([tag[pos + 4], tag[pos + 5], tag[pos + 6], tag[pos + 7]]) as usize
        };
        let start = pos + 10;
        let end = start.checked_add(len)?.min(tag.len());
        if fid == id {
            return Some(tag[start..end].to_vec());
        }
        pos = end;
    }
    None
}

/// Parses a `SYLT` frame payload. Entries that start with a newline begin a
/// new line; others continue the current line as timed words (karaoke-style
/// files). When no entry starts with a newline, every entry is a line.
pub fn parse_sylt(p: &[u8]) -> Option<Lyrics> {
    if p.len() < 6 {
        return None;
    }
    let encoding = p[0];
    let format = p[4]; // 1 = MPEG frames, 2 = milliseconds
    if format != 2 {
        return None;
    }
    let mut pos = 6;
    // Content descriptor, then (text, u32 time) pairs.
    let (_, n) = read_string(&p[pos..], encoding)?;
    pos += n;
    let mut entries: Vec<(String, u64)> = Vec::new();
    while pos < p.len() {
        let (text, n) = read_string(&p[pos..], encoding)?;
        pos += n;
        if pos + 4 > p.len() {
            break;
        }
        let t = u32::from_be_bytes([p[pos], p[pos + 1], p[pos + 2], p[pos + 3]]);
        pos += 4;
        entries.push((text, u64::from(t)));
    }
    if entries.is_empty() {
        return None;
    }
    let word_mode = entries
        .iter()
        .skip(1)
        .any(|(t, _)| t.starts_with('\n') || t.starts_with('\r'));
    let mut lines: Vec<Line> = Vec::new();
    for (text, t) in entries {
        let starts_line = text.starts_with('\n') || text.starts_with('\r');
        let clean = text.trim_start_matches(['\r', '\n']).to_string();
        if word_mode
            && !starts_line
            && let Some(line) = lines.last_mut()
        {
            line.text.push_str(&clean);
            line.words.push(Word {
                start_ms: t,
                text: clean,
            });
            continue;
        }
        lines.push(Line {
            time_ms: Some(t),
            text: clean.clone(),
            words: if word_mode {
                vec![Word {
                    start_ms: t,
                    text: clean,
                }]
            } else {
                Vec::new()
            },
            end_ms: None,
        });
    }
    for l in &mut lines {
        l.text = l.text.trim().to_string();
        if l.words.len() < 2 {
            l.words.clear();
        }
    }
    lines.sort_by_key(|l| l.time_ms);
    Some(Lyrics {
        lines,
        synced: true,
        source: None,
    })
}

/// Reads a terminated ID3 string. Returns the text and bytes consumed.
fn read_string(b: &[u8], encoding: u8) -> Option<(String, usize)> {
    match encoding {
        0 | 3 => {
            let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
            let s = if encoding == 3 {
                String::from_utf8_lossy(&b[..end]).into_owned()
            } else {
                b[..end].iter().map(|&x| char::from(x)).collect()
            };
            Some((s, (end + 1).min(b.len())))
        }
        1 | 2 => {
            let mut end = 0;
            while end + 1 < b.len() && !(b[end] == 0 && b[end + 1] == 0) {
                end += 2;
            }
            let mut body = &b[..end.min(b.len())];
            let mut le = encoding == 1;
            match body {
                [0xFF, 0xFE, rest @ ..] => {
                    le = true;
                    body = rest;
                }
                [0xFE, 0xFF, rest @ ..] => {
                    le = false;
                    body = rest;
                }
                _ => {}
            }
            let units: Vec<u16> = body
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
            Some((String::from_utf16_lossy(&units), (end + 2).min(b.len())))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sylt_utf8(entries: &[(&str, u32)]) -> Vec<u8> {
        let mut p = vec![3, b'e', b'n', b'g', 2, 1];
        p.extend_from_slice(b"desc\0");
        for (text, t) in entries {
            p.extend_from_slice(text.as_bytes());
            p.push(0);
            p.extend_from_slice(&t.to_be_bytes());
        }
        p
    }

    #[test]
    fn sylt_lines() {
        let l = parse_sylt(&sylt_utf8(&[("तुम ही हो", 1_000), ("Second", 3_500)])).expect("sylt");
        assert!(l.synced);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].text, "तुम ही हो");
        assert_eq!(l.lines[1].time_ms, Some(3_500));
        assert!(!l.has_words());
    }

    #[test]
    fn sylt_karaoke_words() {
        let l = parse_sylt(&sylt_utf8(&[
            ("Hel", 1_000),
            ("lo ", 1_300),
            ("world", 1_600),
            ("\nNext", 3_000),
        ]))
        .expect("sylt");
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].text, "Hello world");
        assert_eq!(l.lines[0].words.len(), 3);
        assert_eq!(l.lines[1].text, "Next");
        assert_eq!(l.word_progress(0, 1_450), (1, 0.5));
    }

    #[test]
    fn sylt_utf16_and_frames_format() {
        let mut p = vec![1, b'e', b'n', b'g', 2, 1, 0xFF, 0xFE, 0, 0];
        for (text, t) in [("മഴ", 500u32)] {
            p.extend_from_slice(&[0xFF, 0xFE]);
            for u in text.encode_utf16() {
                p.extend_from_slice(&u.to_le_bytes());
            }
            p.extend_from_slice(&[0, 0]);
            p.extend_from_slice(&t.to_be_bytes());
        }
        let l = parse_sylt(&p).expect("sylt");
        assert_eq!(l.lines[0].text, "മഴ");
        assert_eq!(l.lines[0].time_ms, Some(500));
        p[4] = 1; // MPEG frames: not supported
        assert!(parse_sylt(&p).is_none());
    }

    #[test]
    fn reads_sylt_from_an_id3_tag() {
        let payload = sylt_utf8(&[("Line one", 2_000)]);
        let mut frame = b"SYLT".to_vec();
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes()); // v2.3: plain size
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&payload);
        let size = frame.len() + 16; // with padding
        let ss = [
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ];
        let mut file = b"ID3\x03\x00\x00".to_vec();
        file.extend_from_slice(&ss);
        file.extend_from_slice(&frame);
        file.extend_from_slice(&[0u8; 16]);
        file.extend_from_slice(b"\xff\xfb fake mp3 data");
        let path = std::env::temp_dir().join(format!("lumen-sylt-{}.mp3", std::process::id()));
        std::fs::write(&path, &file).expect("w");
        let l = read_embedded(&path).expect("embedded");
        assert_eq!(l.source, Some(Source::EmbeddedSynced));
        assert_eq!(l.lines[0].text, "Line one");
        let _ = std::fs::remove_file(&path);
    }
}
