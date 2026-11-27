//! Songs screen, plus the song table pieces the album and artist views reuse.
//!
//! The Songs table is a `uniform_list`: only the visible rows are built, so
//! 10,000+ rows scroll as smoothly as 100. Below 900px of width the table
//! scrolls sideways.

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, ClickEvent, Context, Div, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, SharedString, Stateful, StatefulInteractiveElement,
    Styled, Window, div, px, relative, uniform_list,
};
use lumen_core::{SongFilter, SongSort, SortDir, Track};

use crate::app::{LumenApp, PlaySelected, Popup, SONG_TABLE, Screen, SelectNext, SelectPrev};
use crate::assets::Icon;
use crate::popup;
use crate::theme::Theme;
use crate::ui::{
    FontExt, format_count, format_date, format_size, format_time, icon, plural, tracked_label,
};
use crate::widgets::{accent_button, chip, page_title, secondary_button, select, subline};

pub const ROW_H: f32 = 44.0;
pub const HEADER_H: f32 = 40.0;
/// Below this width the table scrolls horizontally.
pub const MIN_TABLE_W: f32 = 900.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    Num,
    Title,
    Artist,
    Album,
    Format,
    Time,
    Modified,
    Plays,
}

pub const COLS: [Col; 8] = [
    Col::Num,
    Col::Title,
    Col::Artist,
    Col::Album,
    Col::Format,
    Col::Time,
    Col::Modified,
    Col::Plays,
];

impl Col {
    fn label(self) -> &'static str {
        match self {
            Col::Num => "#",
            Col::Title => "Title",
            Col::Artist => "Artist",
            Col::Album => "Album",
            Col::Format => "Format",
            Col::Time => "Time",
            Col::Modified => "Modified",
            Col::Plays => "Plays",
        }
    }

    /// The sort a header click picks, for sortable columns.
    fn sort(self) -> Option<SongSort> {
        match self {
            Col::Title => Some(SongSort::Title),
            Col::Artist => Some(SongSort::Artist),
            Col::Album => Some(SongSort::Album),
            Col::Time => Some(SongSort::Duration),
            Col::Modified => Some(SongSort::DateModified),
            Col::Plays => Some(SongSort::PlayCount),
            Col::Num | Col::Format => None,
        }
    }

    fn right_aligned(self) -> bool {
        matches!(self, Col::Time | Col::Plays)
    }

    /// Cell box: fixed widths, or fractions of the remaining width.
    fn cell(self) -> Div {
        let d = div().flex().flex_row().items_center().min_w_0();
        let d = match self {
            Col::Num => d.w(px(48.)).flex_none(),
            Col::Title => grow(d, 2.2),
            Col::Artist | Col::Album => grow(d, 1.4),
            Col::Format => d.w(px(96.)).flex_none(),
            Col::Time => d.w(px(72.)).flex_none(),
            Col::Modified => d.w(px(120.)).flex_none(),
            Col::Plays => d.w(px(64.)).flex_none(),
        };
        if self.right_aligned() {
            d.justify_end()
        } else {
            d
        }
    }
}

fn grow(mut d: Div, fr: f32) -> Div {
    let s = d.style();
    s.flex_grow = Some(fr);
    s.flex_shrink = Some(1.);
    s.flex_basis = Some(relative(0.).into());
    d
}

/// How the # column numbers rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numbering {
    /// Position in the list (Songs).
    Position,
    /// The track number tag (album view).
    TrackNo,
}

/// Short format label and whether it is hi-res: "FLAC", "FLAC 24/96", "MP3".
pub fn format_label(t: &Track) -> (String, bool) {
    let hires = t.is_lossless() && t.is_hires();
    if hires && let (Some(bits), Some(rate)) = (t.bit_depth, t.sample_rate) {
        (format!("{} {bits}/{}", t.codec, khz(rate)), true)
    } else {
        (t.codec.clone(), false)
    }
}

/// `96000` → "96", `44100` → "44.1".
pub fn khz(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        (rate / 1000).to_string()
    } else {
        format!("{:.1}", rate as f64 / 1000.0)
    }
}

/// The small format badge (accent style for hi-res).
fn format_badge(t: &Track, theme: &Theme) -> Div {
    let (label, hires) = format_label(t);
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(20.))
        .px(px(8.))
        .rounded_full()
        .border_1()
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .tabular_nums()
        .map(|d| {
            if hires {
                d.bg(theme.accent_bg_2)
                    .border_color(theme.accent_border)
                    .text_color(theme.accent)
            } else {
                d.border_color(theme.border_soft).text_color(theme.text_3)
            }
        })
        .child(label)
}

/// The sticky header row. `sort` makes the sortable headers clickable.
pub fn header(
    sort: Option<(SongSort, SortDir)>,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let mut cells: Vec<AnyElement> = Vec::with_capacity(COLS.len());
    for col in COLS {
        let active = sort.is_some_and(|(s, _)| col.sort() == Some(s));
        let label: SharedString = match (active, sort) {
            (true, Some((_, dir))) => format!("{} {}", col.label(), dir.arrow()).into(),
            _ => col.label().into(),
        };
        let color = if active { t.accent } else { t.text_muted };
        let text = tracked_label(label, px(11.), 0.06, FontWeight::SEMIBOLD, color);
        let cell = col.cell().h_full().child(text);
        let cell = match (sort, col.sort()) {
            (Some(_), Some(s)) => cell
                .id(("song-col", col as usize))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.click_song_column(s, cx)))
                .into_any_element(),
            _ => cell.into_any_element(),
        };
        cells.push(cell);
    }
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(16.))
        .h(px(HEADER_H))
        .px(px(16.))
        .bg(t.surface)
        .border_b_1()
        .border_color(t.border)
        .children(cells)
}

/// One song row. `list` is the list it belongs to (it becomes the queue on
/// double-click or Enter); `pos` is its position in that list.
pub fn row(
    app: &LumenApp,
    list: &Arc<Vec<usize>>,
    pos: usize,
    numbering: Numbering,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> Stateful<Div> {
    let c = &app.library.catalog;
    let ix = list[pos];
    let track = c.track(ix);
    let id = track.id;
    let playing = app.playing == Some(id);
    let selected = app.songs.selected == Some(id);
    let hover = t.surface_hover;

    let number: AnyElement = if playing {
        icon(Icon::AudioLines, px(16.), t.accent).into_any_element()
    } else {
        let n = match numbering {
            Numbering::Position => (pos + 1).to_string(),
            Numbering::TrackNo => track.track_no.map(|n| n.to_string()).unwrap_or_default(),
        };
        div()
            .tabular_nums()
            .text_size(px(13.))
            .text_color(t.text_muted)
            .child(n)
            .into_any_element()
    };
    let text = |s: &str, size: f32, color| {
        div()
            .min_w_0()
            .truncate()
            .text_size(px(size))
            .text_color(color)
            .child(SharedString::from(s.to_string()))
    };
    let meta = |s: String| {
        div()
            .tabular_nums()
            .text_size(px(13.))
            .text_color(t.text_3)
            .child(s)
    };

    let list_for_click = list.clone();
    let list_for_menu = list.clone();
    div()
        .id(("song-row", ix))
        .w_full()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(16.))
        .h(px(ROW_H))
        .px(px(16.))
        .border_b_1()
        .border_color(t.border_row)
        .map(|d| {
            if playing {
                d.bg(t.playing_row_bg)
            } else if selected {
                d.bg(t.surface_2)
            } else {
                d.hover(move |s| s.bg(hover))
            }
        })
        .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
            window.focus(&this.table_focus);
            this.songs.selected = Some(id);
            if e.click_count() >= 2 {
                this.play_rows(&list_for_click, pos, false, cx);
            }
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                let _ = &list_for_menu;
                window.focus(&this.table_focus);
                this.songs.selected = Some(id);
                this.popup = Some(Popup::Song {
                    track: id,
                    at: e.position,
                    submenu: false,
                    entry: None,
                });
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(Col::Num.cell().child(number))
        .child(
            Col::Title.cell().child(
                text(&track.title, 14., if playing { t.accent } else { t.text })
                    .font_weight(FontWeight::MEDIUM),
            ),
        )
        .child(Col::Artist.cell().child(text(&track.artist, 13., t.text_2)))
        .child(Col::Album.cell().child(text(&track.album, 13., t.text_3)))
        .child(Col::Format.cell().child(format_badge(track, t)))
        .child(
            Col::Time
                .cell()
                .child(meta(format_time(track.duration_ms / 1000))),
        )
        .child(
            Col::Modified
                .cell()
                .child(meta(format_date(track.mtime_ms))),
        )
        .child(
            Col::Plays
                .cell()
                .child(meta(format_count(u64::from(track.play_count)))),
        )
}

/// Wraps a table so it takes keyboard focus: Enter plays, ↑/↓ select.
pub fn focusable_table(
    d: Stateful<Div>,
    focus: &FocusHandle,
    cx: &mut Context<LumenApp>,
) -> Stateful<Div> {
    d.track_focus(focus)
        .key_context(SONG_TABLE)
        .on_action(cx.listener(|this, _: &PlaySelected, _, cx| this.play_selected(cx)))
        .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
        .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
}

/// A bordered radius-12 table container.
pub fn table_frame(t: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .rounded(px(12.))
        .border_1()
        .border_color(t.border)
        .overflow_hidden()
}

impl LumenApp {
    /// The song list Enter and ↑/↓ act on: whatever table is on screen.
    pub fn visible_list(&self) -> Option<Arc<Vec<usize>>> {
        let c = &self.library.catalog;
        match self.screen {
            Screen::Songs => Some(self.songs.rows.clone()),
            Screen::Albums => {
                let a = self.album_index(self.albums.open?)?;
                Some(Arc::new(c.albums[a].tracks.clone()))
            }
            Screen::Artists => {
                if let Some(id) = self.artists.open_album {
                    let a = self.album_index(id)?;
                    Some(Arc::new(c.albums[a].tracks.clone()))
                } else {
                    let a = self.artist_index(self.artists.open?)?;
                    Some(Arc::new(c.artists[a].tracks.clone()))
                }
            }
            Screen::Playlists => Some(self.playlists.tracks.clone()),
            _ => None,
        }
    }

    fn selected_pos(&self, list: &[usize]) -> Option<usize> {
        let id = self.songs.selected?;
        let i = self.library.catalog.index_of(id)?;
        list.iter().position(|&x| x == i)
    }

    pub fn play_selected(&mut self, cx: &mut Context<Self>) {
        let Some(list) = self.visible_list() else {
            return;
        };
        if let Some(pos) = self.selected_pos(&list) {
            self.play_rows(&list, pos, false, cx);
        }
    }

    pub fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(list) = self.visible_list() else {
            return;
        };
        if list.is_empty() {
            return;
        }
        let pos = match self.selected_pos(&list) {
            Some(p) => (p as isize + delta).clamp(0, list.len() as isize - 1) as usize,
            None => 0,
        };
        self.songs.selected = Some(self.library.catalog.track(list[pos]).id);
        if self.screen == Screen::Songs {
            self.songs
                .scroll
                .scroll_to_item(pos, gpui::ScrollStrategy::Top);
        }
        cx.notify();
    }
}

/// "2,418 tracks · 168 GB · 1,902 with synced lyrics".
fn songs_subline(app: &LumenApp) -> String {
    let s = app.songs.stats;
    let mut parts = vec![plural(s.tracks, "track")];
    if s.tracks > 0 {
        parts.push(format_size(s.bytes));
        parts.push(format!(
            "{} with synced lyrics",
            format_count(s.with_synced_lyrics)
        ));
    }
    if let Some((done, total)) = app.library.scan
        && total > 0
    {
        parts.push(format!(
            "scanning {} / {}",
            format_count(done as u64),
            format_count(total as u64)
        ));
    }
    parts.join(" · ")
}

pub fn render(
    app: &mut LumenApp,
    t: &Theme,
    _window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let l = app.settings.library.clone();
    let rows = app.songs.rows.clone();
    let sort_open = app.popup == Some(Popup::SongSort);

    let header_row = div()
        .flex()
        .flex_row()
        .items_end()
        .justify_between()
        .gap(px(24.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .min_w_0()
                .child(page_title("Songs", t))
                .child(subline(songs_subline(app), t)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .relative()
                        .child(
                            select(
                                "song-sort",
                                Some("Sort by"),
                                l.song_sort.label(),
                                sort_open,
                                t,
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.open_popup(Popup::SongSort, cx)),
                            ),
                        )
                        .when(sort_open, |d| {
                            d.child(popup::dropdown(popup::options_menu(
                                "song-sort-opt",
                                SongSort::ALL.iter().map(|s| (s.label(), *s == l.song_sort)),
                                t,
                                cx,
                                move |this, i, cx| {
                                    let s = SongSort::ALL[i];
                                    let dir = this.settings.library.song_sort_dir;
                                    this.popup = None;
                                    this.set_song_sort(s, dir, cx);
                                },
                            )))
                        }),
                )
                .child(
                    secondary_button("song-dir", None, l.song_sort_dir.button_label(), true, t)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let l = &this.settings.library;
                            let (s, d) = (l.song_sort, l.song_sort_dir.flip());
                            this.set_song_sort(s, d, cx);
                        })),
                )
                .child({
                    let enabled = !rows.is_empty();
                    accent_button("play-all", Some(Icon::Play), "Play all", enabled, t).when(
                        enabled,
                        |b| {
                            b.on_click(cx.listener(|this, _, _, cx| {
                                let rows = this.songs.rows.clone();
                                this.play_rows(&rows, 0, false, cx);
                            }))
                        },
                    )
                }),
        );

    let mut chips = Vec::new();
    for f in SongFilter::ALL {
        chips.push(
            chip(("chip", f as usize), f.label(), app.songs.filter == f, t)
                .on_click(cx.listener(move |this, _, _, cx| this.set_song_filter(f, cx))),
        );
    }

    let body: AnyElement = if rows.is_empty() {
        empty_state(app, t, cx).into_any_element()
    } else {
        let list = rows.clone();
        let table = uniform_list(
            "songs",
            rows.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let started = std::time::Instant::now();
                let t = crate::theme::Theme::global(cx).clone();
                let rows = range
                    .map(|pos| row(this, &list, pos, Numbering::Position, &t, cx))
                    .collect::<Vec<_>>();
                crate::ui::profile_log("song rows", started);
                rows
            }),
        )
        .flex_1()
        .track_scroll(app.songs.scroll.clone());

        focusable_table(
            div().id("songs-hscroll").size_full().overflow_x_scroll(),
            &app.table_focus,
            cx,
        )
        .child(
            div()
                .flex()
                .flex_col()
                .min_w(px(MIN_TABLE_W))
                .w_full()
                .h_full()
                .child(header(Some((l.song_sort, l.song_sort_dir)), t, cx))
                .child(table),
        )
        .into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .size_full()
        .pt(px(32.))
        .pb(px(24.))
        .px(px(40.))
        .bg(t.bg)
        .child(header_row)
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(8.))
                .mt(px(20.))
                .children(chips),
        )
        .child(table_frame(t).flex_1().min_h_0().mt(px(20.)).child(body))
}

fn empty_state(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    let (title, note, settings_button): (String, String, bool) = if app.library.folders.is_empty() {
        (
            "No music yet".into(),
            "Add the folder that holds your music in Settings › Library.".into(),
            true,
        )
    } else if app.library.scan.is_some() && app.library.catalog.is_empty() {
        (
            "Reading your music…".into(),
            "Songs appear here as they are found.".into(),
            false,
        )
    } else if !app.query.is_empty() {
        (
            format!("No songs match “{}”", app.query),
            "Try fewer words, or clear the search with Esc.".into(),
            false,
        )
    } else {
        (
            "No songs match this filter".into(),
            "Pick another chip above.".into(),
            false,
        )
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .size_full()
        .p(px(32.))
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text_2)
                .child(title),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(t.text_muted)
                .child(note),
        )
        .when(settings_button, |d| {
            d.child(
                div().mt(px(8.)).child(
                    secondary_button(
                        "open-settings",
                        Some(Icon::FolderPlus),
                        "Add folder",
                        true,
                        t,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                ),
            )
        })
}

/// Rows of a small (non-virtualized) table: album tracks, artist songs.
pub fn static_rows(
    app: &LumenApp,
    list: &Arc<Vec<usize>>,
    numbering: Numbering,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> Vec<Stateful<Div>> {
    (0..list.len())
        .map(|pos| row(app, list, pos, numbering, t, cx))
        .collect()
}
