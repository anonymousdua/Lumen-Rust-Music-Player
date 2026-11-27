//! Albums screen: a responsive grid of covers (virtualized by rows) and the
//! album detail view, which the Artists screen reuses.

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement, Styled, Window, div, px,
    uniform_list,
};
use lumen_core::AlbumSort;

use crate::app::{LumenApp, Popup};
use crate::assets::Icon;
use crate::cover::cover;
use crate::popup;
use crate::songs::{self, MIN_TABLE_W, Numbering};
use crate::theme::Theme;
use crate::ui::{format_long_duration, plural, section_label};
use crate::widgets::{accent_button, back_button, page_title, secondary_button, select, subline};

const CARD_MIN: f32 = 176.0;
const GAP: f32 = 20.0;
const CARD_PAD: f32 = 10.0;

/// Width of the library screens' content (window minus sidebar and padding).
pub fn content_width(window: &Window) -> Pixels {
    (window.viewport_size().width - px(crate::sidebar::WIDTH + 1.0) - px(80.0)).max(px(200.))
}

/// How many cards fit in `width`, and how wide each is.
pub fn grid_columns(width: Pixels, min: f32, gap: f32) -> (usize, Pixels) {
    let w = f32::from(width);
    let cols = (((w + gap) / (min + gap)).floor() as usize).max(1);
    let card = ((w - gap * (cols as f32 - 1.0)) / cols as f32).floor() - 1.0;
    (cols, px(card.max(min.min(w))))
}

/// One album card: cover, title, artist, "2024 · 11 tracks".
pub fn album_card(
    app: &LumenApp,
    album_ix: usize,
    width: Pixels,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> Stateful<gpui::Div> {
    let a = &app.library.catalog.albums[album_ix];
    let id = a.id;
    let hover = t.surface_hover;
    let meta = match a.year {
        Some(y) => format!("{y} · {}", plural(a.track_count as u64, "track")),
        None => plural(a.track_count as u64, "track"),
    };
    let art = width - px(CARD_PAD * 2.0);
    div()
        .id(("album-card", id as usize))
        .flex()
        .flex_col()
        .flex_none()
        .w(width)
        .p(px(CARD_PAD))
        .rounded(px(12.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_click(cx.listener(move |this, _, _, cx| this.open_album(id, cx)))
        .on_mouse_down(
            gpui::MouseButton::Right,
            cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                this.popup = Some(Popup::Album { id, at: e.position });
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(cover(
            a.cover_hash.as_deref(),
            &app.library.paths.thumbs,
            &a.title,
            art,
            px(10.),
            t,
        ))
        .child(
            div()
                .mt(px(10.))
                .h(px(20.))
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .truncate()
                .child(SharedString::from(a.title.clone())),
        )
        .child(
            div()
                .h(px(18.))
                .text_size(px(13.))
                .text_color(t.text_3)
                .truncate()
                .child(SharedString::from(a.artist.clone())),
        )
        .child(
            div()
                .h(px(16.))
                .text_size(px(12.))
                .text_color(t.text_muted)
                .truncate()
                .child(meta),
        )
}

/// Album cards laid out in rows of `cols` (for non-virtualized grids).
pub fn album_rows(
    app: &LumenApp,
    albums: &[usize],
    width: Pixels,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> Vec<AnyElement> {
    let (cols, card) = grid_columns(width + px(2. * CARD_PAD), CARD_MIN, GAP - 2. * CARD_PAD);
    albums
        .chunks(cols)
        .map(|chunk| {
            div()
                .flex()
                .flex_row()
                .gap(px(GAP - 2. * CARD_PAD))
                .children(chunk.iter().map(|&a| album_card(app, a, card, t, cx)))
                .into_any_element()
        })
        .collect()
}

pub fn render(
    app: &mut LumenApp,
    t: &Theme,
    window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    if let Some(ix) = app.albums.open.and_then(|id| app.album_index(id)) {
        let scroll = app.albums.detail_scroll.clone();
        return detail(app, ix, "Albums".into(), &scroll, t, window, cx).into_any_element();
    }
    grid(app, t, window, cx).into_any_element()
}

fn grid(
    app: &mut LumenApp,
    t: &Theme,
    window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let l = app.settings.library.clone();
    let rows = app.albums.rows.clone();
    let sort_open = app.popup == Some(Popup::AlbumSort);
    // Cards carry their own 10px padding so hover fills around the cover:
    // lay out the padded boxes with the gap shrunk to match.
    let width = content_width(window);
    let (cols, card) = grid_columns(width + px(2. * CARD_PAD), CARD_MIN, GAP - 2. * CARD_PAD);
    let row_count = rows.len().div_ceil(cols);

    let header = div()
        .flex()
        .flex_row()
        .items_end()
        .justify_between()
        .gap(px(24.))
        .px(px(40.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .min_w_0()
                .child(page_title("Albums", t))
                .child(subline(plural(rows.len() as u64, "album"), t)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .gap(px(10.))
                .child(
                    div()
                        .relative()
                        .child(
                            select(
                                "album-sort",
                                Some("Sort by"),
                                l.album_sort.label(),
                                sort_open,
                                t,
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.open_popup(Popup::AlbumSort, cx)),
                            ),
                        )
                        .when(sort_open, |d| {
                            d.child(popup::dropdown(popup::options_menu(
                                "album-sort-opt",
                                AlbumSort::ALL
                                    .iter()
                                    .map(|s| (s.label(), *s == l.album_sort)),
                                t,
                                cx,
                                move |this, i, cx| {
                                    let s = AlbumSort::ALL[i];
                                    this.popup = None;
                                    let dir = if s == this.settings.library.album_sort {
                                        this.settings.library.album_sort_dir
                                    } else {
                                        s.natural_dir()
                                    };
                                    this.set_album_sort(s, dir, cx);
                                },
                            )))
                        }),
                )
                .child(
                    secondary_button("album-dir", None, l.album_sort_dir.button_label(), true, t)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let l = &this.settings.library;
                            let (s, d) = (l.album_sort, l.album_sort_dir.flip());
                            this.set_album_sort(s, d, cx);
                        })),
                ),
        );

    let body: AnyElement = if rows.is_empty() {
        empty(app, "albums", t).into_any_element()
    } else {
        uniform_list(
            "album-grid",
            row_count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let t = Theme::global(cx).clone();
                range
                    .map(|r| {
                        let start = r * cols;
                        let end = (start + cols).min(rows.len());
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(GAP - 2. * CARD_PAD))
                            .px(px(40. - CARD_PAD))
                            .pb(px(GAP - 2. * CARD_PAD))
                            .children(
                                rows[start..end]
                                    .iter()
                                    .map(|&a| album_card(this, a, card, &t, cx)),
                            )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(app.albums.scroll.clone())
        .into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .size_full()
        .pt(px(32.))
        .bg(t.bg)
        .child(header)
        .child(div().flex_1().min_h_0().mt(px(20.)).child(body))
}

/// Empty grid: no library, still scanning, or nothing matches the search.
pub fn empty(app: &LumenApp, what: &str, t: &Theme) -> impl IntoElement + use<> {
    let note = if app.library.folders.is_empty() {
        "Add a music folder in Settings › Library.".to_string()
    } else if app.library.scan.is_some() && app.library.catalog.is_empty() {
        "Reading your music…".to_string()
    } else if !app.query.is_empty() {
        format!("No {what} match “{}”.", app.query)
    } else {
        format!("No {what} yet.")
    };
    div()
        .flex()
        .items_center()
        .justify_center()
        .size_full()
        .pb(px(80.))
        .text_size(px(14.))
        .text_color(t.text_muted)
        .child(note)
}

/// Album detail: large cover, title, artist, year, total time, Play and
/// Shuffle, and its tracks in disc and track order.
pub fn detail(
    app: &LumenApp,
    album_ix: usize,
    back: SharedString,
    scroll: &ScrollHandle,
    t: &Theme,
    window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let c = app.library.catalog.clone();
    let a = &c.albums[album_ix];
    let tracks = Arc::new(a.tracks.clone());
    let artist_id = c.artists.iter().find(|x| x.name == a.artist).map(|x| x.id);
    let mut meta = Vec::new();
    if let Some(y) = a.year {
        meta.push(y.to_string());
    }
    meta.push(plural(a.track_count as u64, "track"));
    meta.push(format_long_duration(a.duration_ms));
    let link = t.accent_hover;
    let width = content_width(window);

    let play_list = tracks.clone();
    let shuffle_list = tracks.clone();
    let header = div()
        .flex()
        .flex_row()
        .items_end()
        .gap(px(28.))
        .mt(px(16.))
        .child(cover(
            a.cover_hash.as_deref(),
            &app.library.paths.thumbs,
            &a.title,
            px(200.),
            px(14.),
            t,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .min_w_0()
                .child(section_label("Album", t.text_muted))
                .child(
                    page_title(a.title.clone(), t)
                        .whitespace_normal()
                        .line_clamp(2),
                )
                .child(
                    div()
                        .id("album-artist")
                        .text_size(px(15.))
                        .text_color(t.text_2)
                        .truncate()
                        .when_some(artist_id, |d, id| {
                            d.cursor_pointer()
                                .hover(move |s| s.text_color(link))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.open_artist(id, cx)),
                                )
                        })
                        .child(SharedString::from(a.artist.clone())),
                )
                .child(subline(meta.join(" · "), t))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(10.))
                        .mt(px(10.))
                        .child(
                            accent_button("album-play", Some(Icon::Play), "Play", true, t)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.play_rows(&play_list, 0, false, cx)
                                })),
                        )
                        .child(
                            secondary_button(
                                "album-shuffle",
                                Some(Icon::Shuffle),
                                "Shuffle",
                                true,
                                t,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.play_rows(&shuffle_list, 0, true, cx),
                            )),
                        ),
                ),
        );

    let rows = songs::static_rows(app, &tracks, Numbering::TrackNo, t, cx);
    let table = songs::focusable_table(
        div()
            .id("album-table")
            .mt(px(28.))
            .w_full()
            .overflow_x_scroll(),
        &app.table_focus,
        cx,
    )
    .child(
        songs::table_frame(t)
            .min_w(px(MIN_TABLE_W))
            .w(width.max(px(MIN_TABLE_W)))
            .child(songs::header(None, t, cx))
            .children(rows),
    );

    div()
        .id(("album-detail", a.id as usize))
        .size_full()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .pt(px(24.))
        .pb(px(32.))
        .px(px(40.))
        .bg(t.bg)
        .child(
            back_button("album-back", back, t).on_click(cx.listener(|this, _, window, cx| {
                this.back(window, cx);
            })),
        )
        .child(header)
        .child(table)
}
