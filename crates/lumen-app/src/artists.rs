//! Artists screen: a grid of initials avatars (virtualized by rows) and the
//! artist view (their albums, then all their songs).

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, div, px, uniform_list,
};
use lumen_core::ArtistSort;
use lumen_library::Artist;
use unicode_segmentation::UnicodeSegmentation;

use crate::albums::{self, content_width, grid_columns};
use crate::app::{LumenApp, Popup};
use crate::assets::Icon;
use crate::popup;
use crate::songs::{self, MIN_TABLE_W, Numbering};
use crate::theme::Theme;
use crate::ui::{
    FontExt, format_count, format_last_played, format_long_duration, now_ms, plural, section_label,
};
use crate::widgets::{accent_button, back_button, page_title, secondary_button, select, subline};

const CARD_MIN: f32 = 280.0;
const GAP: f32 = 12.0;
const CARD_H: f32 = 64.0 + 28.0;

/// "Los Ñandúes" → "LÑ", "अरिजीत" → "अ", "The Lumens" → "TL".
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| {
            // First letter-like grapheme of the word (skips "(" and quotes).
            w.graphemes(true)
                .find(|g| g.chars().next().is_some_and(char::is_alphanumeric))
        })
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// 64px (or `size`) circle with initials, colored from the palette.
pub fn avatar(name: &str, size: Pixels, t: &Theme) -> impl IntoElement + use<> {
    let c = t.art_colors_for(name);
    let text = initials(name);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded_full()
        .bg(c.bg)
        .border_1()
        .border_color(t.art_placeholder_border)
        .display_font()
        .text_size(size * (22.0 / 64.0))
        .font_weight(FontWeight::BOLD)
        .text_color(c.fg)
        .child(text)
}

fn counts_line(a: &Artist) -> String {
    format!(
        "{} · {}",
        plural(a.albums.len() as u64, "album"),
        plural(a.track_count as u64, "track")
    )
}

fn plays_line(a: &Artist) -> String {
    match a.last_played_ms {
        Some(ms) if a.plays > 0 => format!(
            "{} · last played {}",
            plural(a.plays, "play"),
            format_last_played(ms, now_ms())
        ),
        _ => "No plays yet".to_string(),
    }
}

fn artist_card(
    app: &LumenApp,
    ix: usize,
    width: Pixels,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> Stateful<gpui::Div> {
    let a = &app.library.catalog.artists[ix];
    let id = a.id;
    let hover = t.surface_hover;
    div()
        .id(("artist-card", id as usize))
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(14.))
        .w(width)
        .h(px(CARD_H))
        .p(px(14.))
        .rounded(px(12.))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_click(cx.listener(move |this, _, _, cx| this.open_artist(id, cx)))
        .child(avatar(&a.name, px(64.), t))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(15.))
                        .line_height(px(21.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .truncate()
                        .child(SharedString::from(a.name.clone())),
                )
                .child(
                    div()
                        .tabular_nums()
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .text_color(t.text_3)
                        .truncate()
                        .child(counts_line(a)),
                )
                .child(
                    div()
                        .tabular_nums()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(t.text_muted)
                        .truncate()
                        .child(plays_line(a)),
                ),
        )
}

pub fn render(
    app: &mut LumenApp,
    t: &Theme,
    window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    if let Some(album) = app.artists.open_album.and_then(|id| app.album_index(id)) {
        let back: SharedString = app
            .artists
            .open
            .and_then(|id| app.artist_index(id))
            .map(|i| app.library.catalog.artists[i].name.clone())
            .unwrap_or_else(|| "Artists".into())
            .into();
        let scroll = app.artists.album_scroll.clone();
        return albums::detail(app, album, back, &scroll, t, window, cx).into_any_element();
    }
    if let Some(ix) = app.artists.open.and_then(|id| app.artist_index(id)) {
        return detail(app, ix, t, window, cx).into_any_element();
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
    let rows = app.artists.rows.clone();
    let sort_open = app.popup == Some(Popup::ArtistSort);
    let (cols, card) = grid_columns(content_width(window), CARD_MIN, GAP);
    let row_count = rows.len().div_ceil(cols);

    let header =
        div()
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
                    .child(page_title("Artists", t))
                    .child(subline(plural(rows.len() as u64, "artist"), t)),
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
                                    "artist-sort",
                                    Some("Sort by"),
                                    l.artist_sort.label(),
                                    sort_open,
                                    t,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.open_popup(Popup::ArtistSort, cx),
                                )),
                            )
                            .when(sort_open, |d| {
                                d.child(popup::dropdown(popup::options_menu(
                                    "artist-sort-opt",
                                    ArtistSort::ALL
                                        .iter()
                                        .map(|s| (s.label(), *s == l.artist_sort)),
                                    t,
                                    cx,
                                    move |this, i, cx| {
                                        let s = ArtistSort::ALL[i];
                                        this.popup = None;
                                        let dir = if s == this.settings.library.artist_sort {
                                            this.settings.library.artist_sort_dir
                                        } else {
                                            s.natural_dir()
                                        };
                                        this.set_artist_sort(s, dir, cx);
                                    },
                                )))
                            }),
                    )
                    .child(
                        secondary_button(
                            "artist-dir",
                            None,
                            l.artist_sort_dir.button_label(),
                            true,
                            t,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let l = &this.settings.library;
                            let (s, d) = (l.artist_sort, l.artist_sort_dir.flip());
                            this.set_artist_sort(s, d, cx);
                        })),
                    ),
            );

    let body: AnyElement = if rows.is_empty() {
        albums::empty(app, "artists", t).into_any_element()
    } else {
        uniform_list(
            "artist-grid",
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
                            .gap(px(GAP))
                            .px(px(40.))
                            .pb(px(GAP))
                            .children(
                                rows[start..end]
                                    .iter()
                                    .map(|&a| artist_card(this, a, card, &t, cx)),
                            )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(app.artists.scroll.clone())
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

/// The artist view: header, their albums, then all their songs.
fn detail(
    app: &LumenApp,
    ix: usize,
    t: &Theme,
    window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let c = app.library.catalog.clone();
    let a = &c.artists[ix];
    let tracks = Arc::new(a.tracks.clone());
    let duration: u64 = a.tracks.iter().map(|&i| c.track(i).duration_ms).sum();
    let width = content_width(window);
    let play_list = tracks.clone();
    let shuffle_list = tracks.clone();

    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(24.))
        .mt(px(16.))
        .child(avatar(&a.name, px(120.), t))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .min_w_0()
                .child(section_label("Artist", t.text_muted))
                .child(page_title(a.name.clone(), t))
                .child(subline(
                    format!("{} · {}", counts_line(a), format_long_duration(duration)),
                    t,
                ))
                .child(subline(plays_line(a), t))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(10.))
                        .mt(px(10.))
                        .child(
                            accent_button("artist-play", Some(Icon::Play), "Play", true, t)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.play_rows(&play_list, 0, false, cx)
                                })),
                        )
                        .child(
                            secondary_button(
                                "artist-shuffle",
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

    let album_grid = albums::album_rows(app, &a.albums, width, t, cx);
    let rows = songs::static_rows(app, &tracks, Numbering::Position, t, cx);
    let table = songs::focusable_table(
        div()
            .id("artist-table")
            .mt(px(12.))
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
        .id(("artist-detail", a.id as usize))
        .size_full()
        .overflow_y_scroll()
        .track_scroll(&app.artists.detail_scroll)
        .pt(px(24.))
        .pb(px(32.))
        .px(px(40.))
        .bg(t.bg)
        .child(
            back_button("artist-back", "Artists", t).on_click(cx.listener(
                |this, _, window, cx| {
                    this.back(window, cx);
                },
            )),
        )
        .child(header)
        .child(div().mt(px(32.)).mb(px(8.)).child(section_label(
            format!("Albums · {}", format_count(a.albums.len() as u64)),
            t.text_muted,
        )))
        .child(div().mx(px(-10.)).children(album_grid))
        .child(div().mt(px(24.)).child(section_label(
            format!("Songs · {}", format_count(a.track_count as u64)),
            t.text_muted,
        )))
        .child(table)
}

#[cfg(test)]
mod tests {
    use super::initials;

    #[test]
    fn initials_from_names() {
        assert_eq!(initials("Los Ñandúes"), "LÑ");
        assert_eq!(initials("The Lumens"), "TL");
        assert_eq!(initials("Rosalía"), "R");
        assert_eq!(initials("अरिजीत"), "अ");
        assert_eq!(initials("യേശുദാസ്"), "യേ");
        assert_eq!(initials("(I'm) Still Here"), "IS");
        assert_eq!(initials("Aurora Fields Band"), "AF");
    }
}
