//! Where the library keeps its data: `%LOCALAPPDATA%\Lumen\`.

use std::path::{Path, PathBuf};

/// Files the library writes.
#[derive(Clone, Debug)]
pub struct LibraryPaths {
    pub db: PathBuf,
    pub thumbs: PathBuf,
}

impl LibraryPaths {
    /// `%LOCALAPPDATA%\Lumen\library.db` and `%LOCALAPPDATA%\Lumen\thumbs\`.
    pub fn default_location() -> Self {
        Self::in_dir(&data_dir())
    }

    pub fn in_dir(dir: &Path) -> Self {
        Self {
            db: dir.join("library.db"),
            thumbs: dir.join("thumbs"),
        }
    }

    /// Path of a cover thumbnail by hash.
    pub fn thumb(&self, hash: &str) -> PathBuf {
        self.thumbs.join(format!("{hash}.jpg"))
    }
}

/// `%LOCALAPPDATA%\Lumen` (falls back to the temp dir if unset).
pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Lumen")
}

/// Case-insensitive "is `path` inside `dir`" (Windows paths ignore case).
pub fn is_within(path: &Path, dir: &Path) -> bool {
    let p = path.to_string_lossy().to_lowercase();
    let d = dir.to_string_lossy().to_lowercase();
    let d = d.trim_end_matches(['\\', '/']);
    p == d || (p.starts_with(d) && matches!(p.as_bytes().get(d.len()), Some(b'\\' | b'/')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within() {
        assert!(is_within(
            Path::new(r"D:\Music\A\b.flac"),
            Path::new(r"d:\music")
        ));
        assert!(is_within(Path::new(r"D:\Music"), Path::new(r"D:\Music\")));
        assert!(!is_within(
            Path::new(r"D:\Musical\b.flac"),
            Path::new(r"D:\Music")
        ));
    }
}
