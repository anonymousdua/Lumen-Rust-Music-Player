//! LRC: `[mm:ss.xx]` line tags (several per line allowed), metadata tags,
//! `[offset:±ms]`, and enhanced word tags `<mm:ss.xx>`.

use crate::{Line, Lyrics, Word};

/// Parses `mm:ss`, `mm:ss.x`, `mm:ss.xx`, `mm:ss.xxx`, `mm:ss:xx` and
/// `h:mm:ss.xx` into milliseconds.
pub(crate) fn parse_time(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty()
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b':' || b == b'.')
    {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    let (h, m, sec_frac): (u64, &str, String) = match parts.as_slice() {
        [m, sf] => (0, m, sf.to_string()),
        [h, m, sf] if sf.contains('.') => (h.parse().ok()?, m, sf.to_string()),
        // "mm:ss:xx": a colon before the hundredths.
        [m, sec, frac] => (0, m, format!("{sec}.{frac}")),
        _ => return None,
    };
    let (sec, frac) = sec_frac.split_once('.').unwrap_or((&sec_frac, ""));
    if m.is_empty() || sec.is_empty() || frac.contains('.') {
        return None;
    }
    let ms_frac = match frac.len() {
        0 => 0,
        1 => frac.parse::<u64>().ok()? * 100,
        2 => frac.parse::<u64>().ok()? * 10,
        _ => frac[..3].parse::<u64>().ok()?,
    };
    Some(
        h * 3_600_000 + m.parse::<u64>().ok()? * 60_000 + sec.parse::<u64>().ok()? * 1000 + ms_frac,
    )
}

/// Splits leading `[...]` tags off a line.
fn leading_tags(line: &str) -> (Vec<&str>, &str) {
    let mut tags = Vec::new();
    let mut rest = line.trim_start();
    while let Some(after) = rest.strip_prefix('[') {
        let Some(end) = after.find(']') else { break };
        tags.push(&after[..end]);
        rest = &after[end + 1..];
    }
    (tags, rest)
}

/// Finds the next `<time>` tag: (start, end of tag, time).
fn next_tag(s: &str) -> Option<(usize, usize, u64)> {
    let mut from = 0;
    while let Some(i) = s[from..].find('<').map(|i| i + from) {
        let close = s[i..].find('>')? + i;
        if let Some(t) = parse_time(&s[i + 1..close]) {
            return Some((i, close + 1, t));
        }
        from = i + 1;
    }
    None
}

/// Splits enhanced word tags out of a line's text. Returns the clean text,
/// the timed words, and the end time if the line ends with a bare tag.
fn words_of(text: &str) -> (String, Vec<Word>, Option<u64>) {
    let mut segs: Vec<(Option<u64>, &str)> = Vec::new();
    let mut current = None;
    let mut rest = text;
    while let Some((i, after, t)) = next_tag(rest) {
        segs.push((current, &rest[..i]));
        current = Some(t);
        rest = &rest[after..];
    }
    segs.push((current, rest));
    let mut end = None;
    if segs.len() > 1
        && let Some((Some(t), last)) = segs.last()
        && last.trim().is_empty()
    {
        end = Some(*t);
    }
    let mut clean = String::new();
    let mut words: Vec<Word> = Vec::new();
    for (start, seg) in segs {
        clean.push_str(seg);
        match start {
            Some(s) if !seg.trim().is_empty() => words.push(Word {
                start_ms: s,
                text: seg.to_string(),
            }),
            // Spaces between tags belong to the previous word.
            _ => {
                if let Some(w) = words.last_mut() {
                    w.text.push_str(seg);
                }
            }
        }
    }
    (clean.trim().to_string(), words, end)
}

/// Parses LRC text. When no line has a time tag, the result is plain lyrics.
pub fn parse_lrc(text: &str) -> Lyrics {
    let mut offset_ms: i64 = 0;
    let mut timed: Vec<Line> = Vec::new();
    let mut plain: Vec<Line> = Vec::new();

    for raw in text.trim_start_matches('\u{feff}').lines() {
        let raw = raw.trim_end_matches('\r');
        let (tags, rest) = leading_tags(raw);
        let mut times = Vec::new();
        for tag in &tags {
            if let Some(t) = parse_time(tag) {
                times.push(t);
            } else if let Some((key, value)) = tag.split_once(':')
                && key.trim().eq_ignore_ascii_case("offset")
            {
                offset_ms = value.trim().replace('+', "").parse().unwrap_or(0);
            }
        }
        if times.is_empty() {
            // Metadata-only lines ("[ar:...]") are not lyrics.
            if tags.is_empty() && !raw.trim().is_empty() {
                plain.push(Line {
                    time_ms: None,
                    text: raw.trim().to_string(),
                    words: Vec::new(),
                    end_ms: None,
                });
            }
            continue;
        }
        let (clean, words, end) = words_of(rest);
        for t in times {
            timed.push(Line {
                time_ms: Some(t),
                text: clean.clone(),
                words: words.clone(),
                end_ms: end,
            });
        }
    }

    if timed.is_empty() {
        return Lyrics {
            lines: plain,
            synced: false,
            source: None,
        };
    }
    // [offset:+500] means the lyrics come 500 ms earlier.
    let shift = |t: u64| (t as i64 - offset_ms).max(0) as u64;
    for l in &mut timed {
        l.time_ms = l.time_ms.map(shift);
        l.end_ms = l.end_ms.map(shift);
        for w in &mut l.words {
            w.start_ms = shift(w.start_ms);
        }
    }
    timed.sort_by_key(|l| l.time_ms);
    Lyrics {
        lines: timed,
        synced: true,
        source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("00:01.00"), Some(1_000));
        assert_eq!(parse_time("01:02.5"), Some(62_500));
        assert_eq!(parse_time("1:02.345"), Some(62_345));
        assert_eq!(parse_time("03:04"), Some(184_000));
        assert_eq!(parse_time("03:04:50"), Some(184_500));
        assert_eq!(parse_time("1:02:03.40"), Some(3_723_400));
        assert_eq!(parse_time("ar"), None);
        assert_eq!(parse_time("ti: Song"), None);
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("1.2.3"), None);
    }

    #[test]
    fn standard_lrc() {
        let l = parse_lrc(
            "\u{feff}[ti:Night]\r\n[ar:Someone]\r\n[00:12.00][01:30.00]Chorus\r\n\
             [00:05.50]मैं हूँ\r\n[00:20.00]\r\n",
        );
        assert!(l.synced);
        let got: Vec<(Option<u64>, &str)> = l
            .lines
            .iter()
            .map(|x| (x.time_ms, x.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (Some(5_500), "मैं हूँ"),
                (Some(12_000), "Chorus"),
                (Some(20_000), ""),
                (Some(90_000), "Chorus"),
            ]
        );
    }

    #[test]
    fn offset_tag_moves_lyrics_earlier() {
        let l = parse_lrc("[offset:+500]\n[00:02.00]a\n[00:00.20]b");
        assert_eq!(l.lines[0].time_ms, Some(0));
        assert_eq!(l.lines[1].time_ms, Some(1_500));
        let l = parse_lrc("[offset:-250]\n[00:02.00]a");
        assert_eq!(l.lines[0].time_ms, Some(2_250));
    }

    #[test]
    fn enhanced_words() {
        let l = parse_lrc("[00:10.00]<00:10.00>നിലാ <00:10.80>വിന്റെ <00:11.60>പാട്ട്<00:12.40>");
        let line = &l.lines[0];
        assert_eq!(line.text, "നിലാ വിന്റെ പാട്ട്");
        let starts: Vec<u64> = line.words.iter().map(|w| w.start_ms).collect();
        assert_eq!(starts, vec![10_000, 10_800, 11_600]);
        assert_eq!(line.words[0].text, "നിലാ ");
        assert_eq!(line.end_ms, Some(12_400));
        let spelled: String = line.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(spelled.trim(), line.text);

        // Words without a closing tag; untimed text before the first tag kept.
        let l = parse_lrc("[00:01.00]Oh <00:01.50>yeah <00:02.00>baby");
        assert_eq!(l.lines[0].text, "Oh yeah baby");
        assert_eq!(l.lines[0].end_ms, None);
        assert_eq!(l.lines[0].words.len(), 2);
        // A "<" that isn't a time tag is just text.
        let l = parse_lrc("[00:01.00]a <3 b");
        assert_eq!(l.lines[0].text, "a <3 b");
        assert!(l.lines[0].words.is_empty());
    }
}
