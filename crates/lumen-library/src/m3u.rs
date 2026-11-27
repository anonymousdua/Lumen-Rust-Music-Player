//! `.m3u` / `.m3u8` playlists: reading (relative, absolute and `file:///`
//! entries) and writing UTF-8 `.m3u8` with paths relative to the playlist.

use std::path::{Component, Path, PathBuf};

/// Paths listed in a playlist, resolved against `base` (the playlist's folder).
pub fn parse(text: &str, base: &Path) -> Vec<PathBuf> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let l = match l.strip_prefix("file:///") {
                Some(rest) => percent_decode(rest),
                None => l.to_string(),
            };
            let p = PathBuf::from(l.replace('/', "\\"));
            normalize(&if p.is_absolute() || p.has_root() {
                p
            } else {
                base.join(p)
            })
        })
        .collect()
}

/// One entry to write.
pub struct ExportItem<'a> {
    pub path: &'a Path,
    pub duration_s: u64,
    pub artist: &'a str,
    pub title: &'a str,
}

/// An `.m3u8` with `#EXTINF` lines and paths relative to `dir` (absolute
/// when the file is on another drive).
pub fn write(items: &[ExportItem<'_>], dir: &Path) -> String {
    let mut out = String::from("#EXTM3U\r\n");
    for it in items {
        out.push_str(&format!(
            "#EXTINF:{},{} - {}\r\n{}\r\n",
            it.duration_s,
            it.artist,
            it.title,
            relative_to(it.path, dir).display()
        ));
    }
    out
}

/// `path` relative to `dir`, compared case-insensitively (Windows); the
/// absolute path when they share no root.
pub fn relative_to(path: &Path, dir: &Path) -> PathBuf {
    let (np, nd) = (normalize(path), normalize(dir));
    let p: Vec<Component> = np.components().collect();
    let d: Vec<Component> = nd.components().collect();
    let same = |a: &Component, b: &Component| {
        a.as_os_str().to_string_lossy().to_lowercase()
            == b.as_os_str().to_string_lossy().to_lowercase()
    };
    let common = p.iter().zip(&d).take_while(|(a, b)| same(a, b)).count();
    // Different drive: keep it absolute.
    if p.is_empty() || d.is_empty() || !same(&p[0], &d[0]) {
        return path.to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..d.len() {
        out.push("..");
    }
    for c in &p[common..] {
        out.push(c.as_os_str());
    }
    out
}

/// Resolves `.` and `..` without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relative_absolute_and_urls() {
        let base = Path::new(r"D:\Music\Playlists");
        let text = "\u{feff}#EXTM3U\r\n#EXTINF:200,A - B\r\n..\\Arijit\\तुम ही हो.flac\r\n\
                    \r\nD:\\Music\\x.mp3\r\nfile:///D:/Music/Ca%C3%B1%C3%B3n%20del%20mar.flac\r\n\
                    sub/one.flac\n";
        let got = parse(text, base);
        assert_eq!(
            got,
            vec![
                PathBuf::from(r"D:\Music\Arijit\तुम ही हो.flac"),
                PathBuf::from(r"D:\Music\x.mp3"),
                PathBuf::from(r"D:\Music\Cañón del mar.flac"),
                PathBuf::from(r"D:\Music\Playlists\sub\one.flac"),
            ]
        );
    }

    #[test]
    fn writes_relative_paths_and_round_trips() {
        let dir = Path::new(r"D:\Music\Playlists");
        let a = PathBuf::from(r"d:\music\ചിത്ര\മഴ.flac");
        let b = PathBuf::from(r"D:\Music\Playlists\here.mp3");
        let c = PathBuf::from(r"E:\Other\far.flac");
        let items = [
            ExportItem {
                path: &a,
                duration_s: 241,
                artist: "ചിത്ര",
                title: "മഴ",
            },
            ExportItem {
                path: &b,
                duration_s: 3,
                artist: "X",
                title: "Here",
            },
            ExportItem {
                path: &c,
                duration_s: 1,
                artist: "Y",
                title: "Far",
            },
        ];
        let text = write(&items, dir);
        assert!(text.starts_with("#EXTM3U\r\n#EXTINF:241,ചിത്ര - മഴ\r\n..\\ചിത്ര\\മഴ.flac\r\n"));
        assert!(text.contains("\r\nhere.mp3\r\n"));
        assert!(text.contains("\r\nE:\\Other\\far.flac\r\n"));
        let back = parse(&text, dir);
        assert_eq!(back[1], b);
        assert_eq!(back[2], c);
        assert_eq!(
            back[0].to_string_lossy().to_lowercase(),
            a.to_string_lossy().to_lowercase()
        );
    }
}
