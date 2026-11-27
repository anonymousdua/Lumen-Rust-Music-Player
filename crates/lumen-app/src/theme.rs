//! Design tokens. Every color a view uses comes from [`Theme`]; views never
//! hard-code colors. Values are exactly the ones in `docs/SPEC.md`.

use gpui::{App, Global, Hsla, Rgba};

/// Converts a `0xRRGGBB` literal into a GPUI color.
const fn hex(rgb: u32) -> Rgba {
    Rgba {
        r: ((rgb >> 16) & 0xff) as f32 / 255.0,
        g: ((rgb >> 8) & 0xff) as f32 / 255.0,
        b: (rgb & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// A background/foreground pair used for placeholder cover art and artist avatars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArtColors {
    pub bg: Hsla,
    pub fg: Hsla,
}

/// All color tokens from the design spec.
///
/// Some tokens are only used by screens built in later batches.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Theme {
    pub bg: Hsla,
    pub bg_titlebar: Hsla,
    pub surface: Hsla,
    pub surface_2: Hsla,
    pub surface_hover: Hsla,
    pub button_hover: Hsla,
    pub nav_hover: Hsla,
    pub border: Hsla,
    pub border_row: Hsla,
    pub border_input: Hsla,
    pub border_soft: Hsla,
    pub track: Hsla,
    pub text: Hsla,
    pub text_2: Hsla,
    pub text_3: Hsla,
    pub text_muted: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub on_accent: Hsla,
    pub accent_bg: Hsla,
    pub accent_bg_2: Hsla,
    pub accent_border: Hsla,
    pub playing_row_bg: Hsla,
    pub switch_knob: Hsla,
    pub art_placeholder: Hsla,
    pub art_placeholder_border: Hsla,
    pub lyric_past: Hsla,
    pub lyric_past_fullscreen: Hsla,
    pub lyric_future: Hsla,
    pub lyric_future_fullscreen: Hsla,
    /// Windows-style red behind the close caption button on hover.
    /// Not in the token table; the spec only says "turns red on hover, as in Windows".
    pub caption_close_hover: Hsla,
    /// Glyph color on top of `caption_close_hover`.
    pub caption_close_glyph: Hsla,
    /// Placeholder art and avatar palette, picked by hashing a name.
    pub art_palette: [ArtColors; 10],
}

impl Global for Theme {}

impl Theme {
    /// The "night listening" dark theme from the spec.
    pub fn night() -> Self {
        let c = |v: u32| -> Hsla { hex(v).into() };
        let pair = |bg: u32, fg: u32| ArtColors {
            bg: c(bg),
            fg: c(fg),
        };
        Self {
            bg: c(0x0B0C0F),
            bg_titlebar: c(0x08090B),
            surface: c(0x0E0F13),
            surface_2: c(0x14161B),
            surface_hover: c(0x15171C),
            button_hover: c(0x1F2128),
            nav_hover: c(0x1A1C22),
            border: c(0x1A1C22),
            border_row: c(0x15171C),
            border_input: c(0x2A2C34),
            border_soft: c(0x23252D),
            track: c(0x23252D),
            text: c(0xE8E6E1),
            text_2: c(0xC9C7C1),
            text_3: c(0xA9A7A1),
            text_muted: c(0x8C8A85),
            accent: c(0xF2B544),
            accent_hover: c(0xFFD27A),
            on_accent: c(0x0B0C0F),
            accent_bg: c(0x1A1812),
            accent_bg_2: c(0x241F12),
            accent_border: c(0x5A4720),
            playing_row_bg: c(0x17150F),
            switch_knob: c(0xF5F3EE),
            art_placeholder: c(0x1B1A17),
            art_placeholder_border: c(0x2A2822),
            lyric_past: c(0x5E5C58),
            lyric_past_fullscreen: c(0x4E4C48),
            lyric_future: c(0x9C9A94),
            lyric_future_fullscreen: c(0x8C8A85),
            caption_close_hover: c(0xC42B1C),
            caption_close_glyph: c(0xFFFFFF),
            art_palette: [
                pair(0x1B1A17, 0xF2B544),
                pair(0x13202A, 0x7FB7D9),
                pair(0x221816, 0xE08A6C),
                pair(0x18172A, 0xA99BE8),
                pair(0x162219, 0x86C79A),
                pair(0x22201A, 0xD9C27F),
                pair(0x24161E, 0xE58FB5),
                pair(0x14201F, 0x79CFC4),
                pair(0x231B12, 0xE3A55A),
                pair(0x1A1B1F, 0xC9C7C1),
            ],
        }
    }

    /// Deterministic placeholder colors for an album or artist name.
    ///
    /// Uses FNV-1a over the lowercased name, so the result is stable across runs
    /// and platforms (unlike `std`'s randomly seeded hasher).
    pub fn art_colors_for(&self, name: &str) -> ArtColors {
        let mut hash: u32 = 0x811c_9dc5;
        for ch in name.chars().flat_map(char::to_lowercase) {
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).bytes() {
                hash ^= u32::from(byte);
                hash = hash.wrapping_mul(0x0100_0193);
            }
        }
        self.art_palette[(hash % self.art_palette.len() as u32) as usize]
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_channels() {
        let c = hex(0xF2B544);
        assert!((c.r - 242.0 / 255.0).abs() < 1e-6);
        assert!((c.g - 181.0 / 255.0).abs() < 1e-6);
        assert!((c.b - 68.0 / 255.0).abs() < 1e-6);
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn art_colors_are_stable_and_case_insensitive() {
        let t = Theme::night();
        assert_eq!(
            t.art_colors_for("Abbey Road"),
            t.art_colors_for("abbey road")
        );
        assert_eq!(t.art_colors_for("ലൂസിഫർ"), t.art_colors_for("ലൂസിഫർ"));
    }
}
