//! Embedded SVG icons (Lucide, ISC license, plus a few hand-drawn caption glyphs).

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        /// Every icon the app can draw. Some are only used by later batches.
        #[allow(dead_code)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon { $($variant),* }

        impl Icon {
            /// Asset path passed to `gpui::svg().path(..)`.
            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $file)),* }
            }
        }

        fn load_icon(path: &str) -> Option<&'static [u8]> {
            match path {
                $(concat!("icons/", $file) => Some(include_bytes!(concat!("../../../assets/icons/", $file)))),*,
                _ => None,
            }
        }

        const ALL_PATHS: &[&str] = &[$(concat!("icons/", $file)),*];
    };
}

icons! {
    Logo => "audio-waveform.svg",
    Search => "search.svg",
    NowPlaying => "disc-3.svg",
    Songs => "music.svg",
    Albums => "disc-album.svg",
    Artists => "mic-vocal.svg",
    Playlists => "list-music.svg",
    Settings => "settings.svg",
    SkipBack => "skip-back.svg",
    SkipForward => "skip-forward.svg",
    Play => "play-filled.svg",
    Pause => "pause-filled.svg",
    Volume => "volume-2.svg",
    AudioLines => "audio-lines.svg",
    CaptionMinimize => "caption-minimize.svg",
    CaptionMaximize => "caption-maximize.svg",
    CaptionRestore => "caption-restore.svg",
    CaptionClose => "caption-close.svg",
    FolderPlus => "folder-plus.svg",
    Folder => "folder.svg",
    Refresh => "refresh-cw.svg",
    Close => "x.svg",
    ChevronDown => "chevron-down.svg",
    ChevronRight => "chevron-right.svg",
    Check => "check.svg",
    Back => "arrow-left.svg",
    Shuffle => "shuffle.svg",
    VolumeMute => "volume-x.svg",
    Repeat => "repeat.svg",
    RepeatOne => "repeat-1.svg",
    Plus => "plus.svg",
    More => "ellipsis.svg",
    Grip => "grip-vertical.svg",
    Smart => "sparkles.svg",
    Radio => "radio.svg",
}

/// Serves the embedded icons to GPUI.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(load_icon(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ALL_PATHS
            .iter()
            .filter(|p| p.starts_with(path))
            .map(|p| SharedString::from(*p))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads() {
        for path in ALL_PATHS {
            let bytes = Assets.load(path).ok().flatten();
            assert!(bytes.is_some_and(|b| b.starts_with(b"<svg")), "{path}");
        }
    }
}
