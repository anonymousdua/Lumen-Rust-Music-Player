//! Small styling helpers shared by every view.

use gpui::{
    AnyElement, Div, FontWeight, Hsla, IntoElement, ParentElement, Pixels, SharedString, Styled,
    Svg, div, px, svg,
};

use crate::assets::Icon;
use crate::fonts;

/// Font helpers for any styled element. Both families carry the Devanagari and
/// Malayalam fallback chain.
pub trait FontExt: Styled + Sized {
    /// Inter, with Noto fallbacks.
    fn ui_font(mut self) -> Self {
        let style = self.text_style().get_or_insert_with(Default::default);
        style.font_family = Some(fonts::UI.into());
        style.font_fallbacks = Some(fonts::fallbacks());
        self
    }

    /// Space Grotesk, with Noto fallbacks.
    fn display_font(mut self) -> Self {
        let style = self.text_style().get_or_insert_with(Default::default);
        style.font_family = Some(fonts::DISPLAY.into());
        style.font_fallbacks = Some(fonts::fallbacks());
        self
    }

    /// The lyrics font picked in Settings, with Noto fallbacks.
    fn lyrics_font(mut self, font: lumen_core::LyricsFont) -> Self {
        let family = match font {
            lumen_core::LyricsFont::SpaceGrotesk => fonts::DISPLAY,
            lumen_core::LyricsFont::SegoeUi => fonts::SEGOE_UI,
            lumen_core::LyricsFont::NotoSans => fonts::NOTO_SANS,
        };
        let style = self.text_style().get_or_insert_with(Default::default);
        style.font_family = Some(family.into());
        style.font_fallbacks = Some(fonts::fallbacks());
        self
    }

    /// Tabular figures (times, durations, track numbers, counts).
    fn tabular_nums(mut self) -> Self {
        let style = self.text_style().get_or_insert_with(Default::default);
        style.font_features = Some(fonts::tabular());
        self
    }
}

impl<T: Styled + Sized> FontExt for T {}

/// A monochrome line icon tinted with `color`.
pub fn icon(icon: Icon, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(icon.path())
        .size(size)
        .flex_none()
        .text_color(color)
}

/// Uppercase label with letter-spacing.
///
/// GPUI 0.2.2 has no letter-spacing style, so each character is laid out as
/// its own text run with a gap of `tracking_em * size` between them. Only use
/// this for short, static labels (section labels, table headers).
pub fn tracked_label(
    text: impl Into<SharedString>,
    size: Pixels,
    tracking_em: f32,
    weight: FontWeight,
    color: Hsla,
) -> Div {
    let text: SharedString = text.into();
    let gap = size * tracking_em;
    div()
        .flex()
        .flex_row()
        .flex_none()
        .gap(gap)
        .ui_font()
        .text_size(size)
        .line_height(size * 1.4)
        .font_weight(weight)
        .text_color(color)
        .children(
            text.to_uppercase()
                .chars()
                .map(|ch| -> AnyElement {
                    if ch == ' ' {
                        // A space is a gap of roughly a quarter em, plus the tracking on both sides.
                        div().w(size * 0.28).into_any_element()
                    } else {
                        SharedString::from(ch.to_string()).into_any_element()
                    }
                })
                .collect::<Vec<_>>(),
        )
}

/// The 11px uppercase section label ("LIBRARY") in `text_muted`.
pub fn section_label(text: impl Into<SharedString>, color: Hsla) -> Div {
    tracked_label(text, px(11.), 0.08, FontWeight::SEMIBOLD, color)
}

/// Formats seconds as `m:ss` (or `h:mm:ss`).
pub fn format_time(total_seconds: u64) -> String {
    let h = total_seconds / 3600;
    let m = (total_seconds % 3600) / 60;
    let s = total_seconds % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `2418` → "2,418".
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// "just now", "2 min ago", "3 h ago", "yesterday", "5 days ago".
pub fn format_ago(then_ms: i64, now_ms: i64) -> String {
    let secs = (now_ms - then_ms).max(0) / 1000;
    match secs {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{} min ago", secs / 60),
        3_600..=86_399 => format!("{} h ago", secs / 3_600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// `1_700_000_000_000` → "14 Nov 2023", in the local time zone.
pub fn format_date(ms: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d) = local_date(ms);
    format!("{d} {} {y}", MONTHS[(m.clamp(1, 12) - 1) as usize])
}

/// Year, month (1-12) and day of `ms` in local time (UTC if that fails).
#[allow(unsafe_code)]
fn local_date(ms: i64) -> (i32, u32, u32) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
        // FILETIME counts 100 ns ticks since 1601-01-01.
        let ticks = (ms.max(0) + 11_644_473_600_000) * 10_000;
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        // SAFETY: plain value conversions on stack structs we own.
        let ok = unsafe {
            FileTimeToSystemTime(&ft, &mut utc).is_ok()
                && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
        };
        if ok {
            return (
                i32::from(local.wYear),
                u32::from(local.wMonth),
                u32::from(local.wDay),
            );
        }
    }
    civil_from_days(ms.div_euclid(86_400_000))
}

/// "Today", "Yesterday", "3 days ago" or "24 Nov 2026" (local dates).
pub fn format_last_played(ms: i64, now_ms: i64) -> String {
    let day = |ms| {
        let (y, m, d) = local_date(ms);
        days_from_civil(y, m, d)
    };
    match day(now_ms) - day(ms) {
        i64::MIN..=0 => "Today".into(),
        1 => "Yesterday".into(),
        n @ 2..=6 => format!("{n} days ago"),
        _ => format_date(ms),
    }
}

/// (year, month, day) → days since 1970-01-01.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

/// `180_388_626_432` → "168 GB" (binary units, as Explorer shows them).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 || v >= 10.0 {
        format!("{} {}", v.round() as u64, UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// "1 h 12 min" / "42 min" / "24 s" for album and artist totals.
pub fn format_long_duration(ms: u64) -> String {
    if ms < 60_000 {
        return format!("{} s", ms / 1000);
    }
    let mins = (ms + 30_000) / 60_000;
    if mins >= 60 {
        format!("{} h {} min", mins / 60, mins % 60)
    } else {
        format!("{mins} min")
    }
}

/// `n` and the noun, pluralized: `plural(1, "track")` → "1 track".
pub fn plural(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{} {noun}s", format_count(n))
    }
}

/// `LUMEN_PROFILE=1`: log slow renders to stderr (for performance checks).
pub fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LUMEN_PROFILE").is_some())
}

/// Logs `what` with its duration when profiling.
pub fn profile_log(what: &str, started: std::time::Instant) {
    if profiling() {
        eprintln!(
            "[profile] {what}: {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        civil_from_days, format_ago, format_count, format_long_duration, format_size, format_time,
        plural,
    };

    #[test]
    fn formats_dates_sizes_and_totals() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_416), (2025, 11, 24));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        for z in [-800_000, -1, 0, 11_016, 20_416, 60_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(super::days_from_civil(y, m, d), z);
        }
        let now = 1_764_000_000_000;
        assert_eq!(super::format_last_played(now - 60_000, now), "Today");
        assert_eq!(
            super::format_last_played(now - 3 * 86_400_000, now),
            "3 days ago"
        );
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(4_404_019), "4.2 MB");
        assert_eq!(format_size(180_388_626_432), "168 GB");
        assert_eq!(format_long_duration(24_000), "24 s");
        assert_eq!(format_long_duration(42 * 60_000), "42 min");
        assert_eq!(format_long_duration(72 * 60_000 + 10_000), "1 h 12 min");
        assert_eq!(plural(1, "track"), "1 track");
        assert_eq!(plural(2_418, "track"), "2,418 tracks");
    }

    #[test]
    fn formats_counts_and_ages() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(2_418), "2,418");
        assert_eq!(format_count(1_234_567), "1,234,567");
        let now = 1_000_000_000;
        assert_eq!(format_ago(now - 5_000, now), "just now");
        assert_eq!(format_ago(now - 120_000, now), "2 min ago");
        assert_eq!(format_ago(now - 3 * 3_600_000, now), "3 h ago");
        assert_eq!(format_ago(now - 30 * 3_600_000, now), "yesterday");
        assert_eq!(format_ago(now - 5 * 86_400_000, now), "5 days ago");
    }

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(84), "1:24");
        assert_eq!(format_time(605), "10:05");
        assert_eq!(format_time(3_725), "1:02:05");
    }
}
