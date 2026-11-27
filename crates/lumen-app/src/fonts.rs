//! Bundled fonts. Everything is compiled into the binary with `include_bytes!`
//! and registered with GPUI at startup, so Lumen never depends on installed fonts.
//! Licenses live in `/licenses`.

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{App, FontFallbacks, FontFeatures};

/// UI font family (400, 500, 600).
pub const UI: &str = "Inter";
/// Display font family (500, 600, 700): titles, lyrics, wordmark.
pub const DISPLAY: &str = "Space Grotesk";
/// Optional lyrics font (600, 700), Latin; Indic scripts use the fallbacks.
pub const NOTO_SANS: &str = "Noto Sans";
/// System font, only when picked as the lyrics font.
pub const SEGOE_UI: &str = "Segoe UI";

/// Families used as fallbacks for both UI and display text so Hindi and
/// Malayalam render everywhere.
const FALLBACKS: [&str; 2] = ["Noto Sans Devanagari", "Noto Sans Malayalam"];

macro_rules! font_files {
    ($($name:literal),* $(,)?) => {
        [$(include_bytes!(concat!("../../../assets/fonts/", $name)).as_slice()),*]
    };
}

static FONT_FILES: [&[u8]; 16] = font_files![
    "Inter-Regular.ttf",
    "Inter-Medium.ttf",
    "Inter-SemiBold.ttf",
    "SpaceGrotesk-Medium.ttf",
    "SpaceGrotesk-SemiBold.ttf",
    "SpaceGrotesk-Bold.ttf",
    "NotoSansDevanagari-Regular.ttf",
    "NotoSansDevanagari-Medium.ttf",
    "NotoSansDevanagari-SemiBold.ttf",
    "NotoSansDevanagari-Bold.ttf",
    "NotoSansMalayalam-Regular.ttf",
    "NotoSansMalayalam-Medium.ttf",
    "NotoSansMalayalam-SemiBold.ttf",
    "NotoSansMalayalam-Bold.ttf",
    "NotoSans-SemiBold.ttf",
    "NotoSans-Bold.ttf",
];

/// Registers every bundled font with GPUI. Call once, before opening a window.
pub fn register(cx: &App) -> gpui::Result<()> {
    let fonts = FONT_FILES
        .iter()
        .map(|bytes| Cow::Borrowed(*bytes))
        .collect();
    cx.text_system().add_fonts(fonts)
}

/// The Devanagari + Malayalam fallback chain shared by both families.
pub fn fallbacks() -> FontFallbacks {
    FontFallbacks(Arc::new(FALLBACKS.iter().map(|s| s.to_string()).collect()))
}

/// OpenType `tnum`: tabular figures for times, durations, numbers and counts.
pub fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}
