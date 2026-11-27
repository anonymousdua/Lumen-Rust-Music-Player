//! Left sidebar: search box, library navigation, settings and the status box.

use gpui::prelude::FluentBuilder;
use gpui::{
    Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::app::{LumenApp, Screen};
use crate::assets::Icon;
use crate::library_model::LibraryModel;
use crate::theme::Theme;
use crate::ui::{FontExt, format_ago, format_count, icon, now_ms, section_label};

pub const WIDTH: f32 = 240.0;

pub fn render(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    let active = app.screen;
    let library = &app.library;
    div()
        .id("sidebar")
        .flex()
        .flex_col()
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .px(px(12.))
        .py(px(20.))
        .bg(t.surface)
        .border_r_1()
        .border_color(t.border)
        .child(search_box(app, t, cx))
        .child(
            div()
                .pt(px(24.))
                .pb(px(8.))
                .px(px(12.))
                .child(section_label("Library", t.text_muted)),
        )
        .child({
            let mut items = Vec::with_capacity(Screen::LIBRARY.len());
            for screen in Screen::LIBRARY {
                items.push(nav_item(screen, active, t, cx));
            }
            div().flex().flex_col().gap(px(2.)).children(items)
        })
        .child(div().flex_1())
        .child(nav_item(Screen::Settings, active, t, cx))
        .child(status_box(library, t))
}

/// "Search library": typing filters the current screen, Enter jumps to
/// Songs, Esc clears, Ctrl+F focuses.
fn search_box(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    div()
        .id("search")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .h(px(38.))
        .px(px(12.))
        .rounded(px(8.))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border_soft)
        .text_size(px(13.))
        .line_height(px(18.))
        .text_color(t.text)
        .cursor_text()
        .on_click(cx.listener(|this, _, window, cx| this.focus_search(window, cx)))
        .child(icon(Icon::Search, px(16.), t.text_muted))
        .child(app.search.clone())
}

fn nav_item(
    screen: Screen,
    active: Screen,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let is_active = screen == active;
    let (fg, bg) = if is_active {
        (t.accent, t.accent_bg)
    } else {
        (t.text_2, gpui::transparent_black())
    };
    let hover_bg = t.nav_hover;

    div()
        .id(screen.id())
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.))
        .h(px(40.))
        .px(px(12.))
        .rounded(px(8.))
        .bg(bg)
        .when(!is_active, move |d| d.hover(move |s| s.bg(hover_bg)))
        .text_color(fg)
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.navigate(screen, cx);
        }))
        .child(icon(screen.icon(), px(18.), fg))
        .child(screen.title())
}

/// Library summary: live track count, and scan progress or the last scan time.
fn status_box(lib: &LibraryModel, t: &Theme) -> impl IntoElement + use<> {
    let tracks = lib.stats.tracks;
    let first = if lib.open_error.is_some() {
        "Library unavailable".to_string()
    } else if tracks == 1 {
        "1 track · local only".to_string()
    } else {
        format!("{} tracks · local only", format_count(tracks))
    };
    let second = if lib.open_error.is_some() {
        "See Settings".to_string()
    } else if let Some((done, total)) = lib.scan {
        if total == 0 {
            "Scanning…".to_string()
        } else {
            format!(
                "Scanning… {} / {}",
                format_count(done as u64),
                format_count(total as u64)
            )
        }
    } else if lib.folders.is_empty() {
        "Add a music folder in Settings".to_string()
    } else {
        match lib.stats.last_scan_ms {
            Some(ms) => format!("Last scan {}", format_ago(ms, now_ms())),
            None => "Not scanned yet".to_string(),
        }
    };
    div()
        .mt(px(12.))
        .p(px(12.))
        .rounded(px(10.))
        .bg(t.surface_2)
        .text_size(px(12.))
        .line_height(px(18.))
        .text_color(t.text_muted)
        .ui_font()
        .tabular_nums()
        .child(first)
        .child(second)
}
