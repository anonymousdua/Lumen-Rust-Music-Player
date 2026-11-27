//! Playlists screen: the playlist list on the left ("Made for you" mixes,
//! smart playlists, then yours), the selected playlist on the right with its sort, Play, and a
//! 48px-row track table. User playlists can be renamed, deleted, reordered
//! by drag (under "Custom order"), and imported/exported as `.m3u8`.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, PathPromptOptions, Pixels, Point,
    Render, SharedString, Stateful, StatefulInteractiveElement, Styled, UniformListScrollHandle,
    Window, div, px, relative, uniform_list,
};
use lumen_core::{PlaylistSort, SortDir};
use lumen_library::SmartPlaylist;
use lumen_reco::MixKind;

use crate::app::{LumenApp, Popup};
use crate::assets::Icon;
use crate::popup;
use crate::songs::{focusable_table, table_frame};
use crate::text_input::{InputEvent, TextInput};
use crate::theme::Theme;
use crate::ui::{FontExt, format_count, format_time, icon, now_ms, plural, tracked_label};
use crate::widgets::{
    accent_button, menu_item, menu_panel, menu_separator, secondary_button, select, text_tooltip,
};

pub const ROW_H: f32 = 48.0;
const LIST_W: f32 = 260.0;

/// Which playlist is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistSel {
    /// A "Made for you" mix (read-only).
    Mix(MixKind),
    Smart(SmartPlaylist),
    User(i64),
}

pub struct PlaylistsState {
    pub selected: PlaylistSel,
    /// Display order: (catalog index, date added, position in the playlist).
    pub rows: Arc<Vec<(usize, i64, usize)>>,
    /// Catalog indices in display order (the queue for Play / double-click).
    pub tracks: Arc<Vec<usize>>,
    pub smart_counts: [usize; 4],
    pub scroll: UniformListScrollHandle,
    /// Title being edited.
    pub rename: Option<Entity<TextInput>>,
    pub confirm_delete: bool,
    /// Select (and rename) the next playlist the library creates.
    pub select_next_created: bool,
    pub rename_next_created: bool,
    /// The next created playlist comes from an import (report the result).
    pub import_pending: bool,
    /// Result of the last import or export.
    pub message: Option<String>,
}

impl Default for PlaylistsState {
    fn default() -> Self {
        Self {
            selected: PlaylistSel::Smart(SmartPlaylist::HiRes),
            rows: Arc::default(),
            tracks: Arc::default(),
            smart_counts: [0; 4],
            scroll: UniformListScrollHandle::new(),
            rename: None,
            confirm_delete: false,
            select_next_created: false,
            rename_next_created: false,
            import_pending: false,
            message: None,
        }
    }
}

/// What is being dragged: a playlist entry by its position.
#[derive(Clone, Debug)]
pub struct DraggedEntry {
    pub position: usize,
    pub title: SharedString,
}

/// The drag preview.
struct DragPreview {
    title: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::global(cx);
        div()
            .px(px(12.))
            .py(px(8.))
            .rounded(px(8.))
            .bg(t.surface_2)
            .border_1()
            .border_color(t.accent_border)
            .shadow_lg()
            .ui_font()
            .text_size(px(14.))
            .text_color(t.text)
            .child(self.title.clone())
    }
}

impl LumenApp {
    /// Rebuilds the shown playlist (after a library change, a new
    /// selection or a new sort).
    pub fn recompute_playlist(&mut self) {
        let c = self.library.catalog.clone();
        let now = now_ms();
        for (k, s) in SmartPlaylist::ALL.iter().enumerate() {
            self.playlists.smart_counts[k] = s.rows(&c, now).len();
        }
        let gone = match self.playlists.selected {
            PlaylistSel::User(id) => !self.library.playlists.iter().any(|p| p.id == id),
            PlaylistSel::Mix(_) => !self.reco_on(),
            PlaylistSel::Smart(_) => false,
        };
        if gone {
            self.playlists.selected = PlaylistSel::Smart(SmartPlaylist::HiRes);
        }
        let mut rows: Vec<(usize, i64, usize)> = match self.playlists.selected {
            PlaylistSel::Mix(kind) => match self.reco.mix(kind) {
                Some(mix) => mix
                    .tracks
                    .iter()
                    .filter_map(|id| c.index_of(*id))
                    .enumerate()
                    .map(|(pos, i)| (i, mix.generated_at, pos))
                    .collect(),
                None => Vec::new(),
            },
            PlaylistSel::Smart(s) => s
                .rows(&c, now)
                .into_iter()
                .enumerate()
                .map(|(pos, (i, added))| (i, added, pos))
                .collect(),
            PlaylistSel::User(id) => self
                .library
                .playlist_entries(id)
                .into_iter()
                .enumerate()
                .filter_map(|(pos, e)| c.index_of(e.track).map(|i| (i, e.added_ms, pos)))
                .collect(),
        };
        let l = &self.settings.library;
        c.sort_playlist(&mut rows, l.playlist_sort, l.playlist_sort_dir);
        self.playlists.tracks = Arc::new(rows.iter().map(|r| r.0).collect());
        self.playlists.rows = Arc::new(rows);
    }

    pub fn select_playlist(&mut self, sel: PlaylistSel, cx: &mut Context<Self>) {
        if self.playlists.selected != sel {
            self.playlists.selected = sel;
            self.playlists.rename = None;
            self.playlists.confirm_delete = false;
            self.playlists.message = None;
            self.recompute_playlist();
            self.playlists
                .scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
        cx.notify();
    }

    pub fn set_playlist_sort(&mut self, sort: PlaylistSort, dir: SortDir, cx: &mut Context<Self>) {
        let l = &mut self.settings.library;
        l.playlist_sort = sort;
        l.playlist_sort_dir = dir;
        self.save_settings();
        self.recompute_playlist();
        cx.notify();
    }

    fn selected_user(&self) -> Option<(i64, String)> {
        match self.playlists.selected {
            PlaylistSel::User(id) => self
                .library
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map(|p| (p.id, p.name.clone())),
            PlaylistSel::Smart(_) | PlaylistSel::Mix(_) => None,
        }
    }

    /// "+" › New playlist: created empty, then selected and renamed.
    pub fn create_playlist(&mut self, cx: &mut Context<Self>) {
        let name = popup::new_playlist_name(self);
        if let Some(l) = self.library.library() {
            self.playlists.select_next_created = true;
            self.playlists.rename_next_created = true;
            l.new_playlist(name, Vec::new());
        }
        cx.notify();
    }

    /// The library created a playlist (new or imported).
    pub fn on_playlist_created(
        &mut self,
        id: i64,
        added: usize,
        missing: usize,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if !std::mem::take(&mut self.playlists.select_next_created) {
            return;
        }
        self.select_playlist(PlaylistSel::User(id), cx);
        if std::mem::take(&mut self.playlists.import_pending) {
            self.playlists.message = Some(format!(
                "Imported {} · {} not in your library",
                plural(added as u64, "track"),
                format_count(missing as u64)
            ));
        }
        if std::mem::take(&mut self.playlists.rename_next_created)
            && let Some(window) = window
        {
            self.start_rename(window, cx);
        }
    }

    pub fn import_playlist(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import playlist".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |app, cx| {
                    if let Some(l) = app.library.library() {
                        app.playlists.select_next_created = true;
                        app.playlists.rename_next_created = false;
                        app.playlists.import_pending = true;
                        l.import_playlist(path);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Writes the playlist (in custom order) as `.m3u8` with paths relative
    /// to where it is saved.
    pub fn export_playlist(&mut self, cx: &mut Context<Self>) {
        let Some((id, name)) = self.selected_user() else {
            return;
        };
        let c = self.library.catalog.clone();
        let items: Vec<(PathBuf, u64, String, String)> = self
            .library
            .playlist_entries(id)
            .into_iter()
            .filter_map(|e| c.index_of(e.track))
            .map(|i| {
                let t = c.track(i);
                (
                    t.path.clone(),
                    t.duration_ms / 1000,
                    t.artist.clone(),
                    t.title.clone(),
                )
            })
            .collect();
        let dir = self
            .library
            .folders
            .first()
            .map(|f| f.path.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let safe: String = name
            .chars()
            .map(|ch| if r#"\/:*?"<>|"#.contains(ch) { '_' } else { ch })
            .collect();
        let picked = cx.prompt_for_new_path(&dir, Some(&format!("{safe}.m3u8")));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let base = path.parent().map(PathBuf::from).unwrap_or_default();
            let export: Vec<lumen_library::m3u::ExportItem<'_>> = items
                .iter()
                .map(|(p, d, a, t)| lumen_library::m3u::ExportItem {
                    path: p,
                    duration_s: *d,
                    artist: a,
                    title: t,
                })
                .collect();
            let text = lumen_library::m3u::write(&export, &base);
            let result = std::fs::write(&path, text);
            let _ = this.update(cx, |app, cx| {
                app.playlists.message = Some(match result {
                    Ok(()) => format!(
                        "Exported {} to {}",
                        plural(items.len() as u64, "track"),
                        path.display()
                    ),
                    Err(e) => format!("Could not save {}: {e}", path.display()),
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, name)) = self.selected_user() else {
            return;
        };
        let input = cx.new(|cx| TextInput::new("Playlist name", cx));
        input.update(cx, |i, cx| {
            i.set_text(name, cx);
            i.focus_and_select_all(window, cx);
        });
        cx.subscribe_in(
            &input,
            window,
            move |this, input, e: &InputEvent, window, cx| match e {
                InputEvent::Submit => {
                    let name = input.read(cx).text().trim().to_string();
                    if !name.is_empty()
                        && let Some(l) = this.library.library()
                    {
                        l.rename_playlist(id, name);
                    }
                    this.playlists.rename = None;
                    window.focus(&this.root_focus);
                    cx.notify();
                }
                InputEvent::Cancel => {
                    this.playlists.rename = None;
                    window.focus(&this.root_focus);
                    cx.notify();
                }
                InputEvent::Changed(_) => {}
            },
        )
        .detach();
        self.playlists.rename = Some(input);
        self.popup = None;
        cx.notify();
    }

    pub fn delete_selected_playlist(&mut self, cx: &mut Context<Self>) {
        if let (Some((id, _)), Some(l)) = (self.selected_user(), self.library.library()) {
            l.delete_playlist(id);
        }
        self.playlists.confirm_delete = false;
        self.playlists.selected = PlaylistSel::Smart(SmartPlaylist::HiRes);
        self.recompute_playlist();
        cx.notify();
    }

    /// Drop: the entry at `from` goes to where `to` is.
    fn drop_entry(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if let (PlaylistSel::User(id), Some(l)) = (self.playlists.selected, self.library.library())
            && from != to
        {
            l.move_in_playlist(id, from, to);
            // Show the move right away; the library confirms it.
            let mut rows: Vec<(usize, i64, usize)> = self.playlists.rows.to_vec();
            let mut custom: Vec<(usize, i64, usize)> = rows.clone();
            custom.sort_by_key(|r| r.2);
            let moved = custom.remove(from);
            custom.insert(to.min(custom.len()), moved);
            for (pos, r) in custom.iter_mut().enumerate() {
                r.2 = pos;
            }
            rows.clear();
            rows.extend(custom);
            let l = &self.settings.library;
            self.library
                .catalog
                .sort_playlist(&mut rows, l.playlist_sort, l.playlist_sort_dir);
            self.playlists.tracks = Arc::new(rows.iter().map(|r| r.0).collect());
            self.playlists.rows = Arc::new(rows);
        }
        cx.notify();
    }
}

/// A playlist button in the left column.
fn list_button(
    id: impl Into<gpui::ElementId>,
    name: impl Into<SharedString>,
    sub: String,
    selected: bool,
    smart: bool,
    t: &Theme,
) -> Stateful<Div> {
    let hover = t.surface_hover;
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(10.))
        .border_1()
        .cursor_pointer()
        .map(|d| {
            if selected {
                d.bg(t.playing_row_bg).border_color(t.accent_border)
            } else {
                d.border_color(gpui::transparent_black())
                    .hover(move |s| s.bg(hover))
            }
        })
        .when(smart, |d| {
            d.child(icon(
                Icon::Smart,
                px(14.),
                if selected { t.accent } else { t.text_muted },
            ))
        })
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if selected { t.accent } else { t.text })
                        .truncate()
                        .child(name.into()),
                )
                .child(
                    div()
                        .tabular_nums()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(t.text_muted)
                        .child(sub),
                ),
        )
}

fn left_column(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    let add_open = app.popup == Some(Popup::PlaylistAdd);
    let hover = t.button_hover;
    let mut buttons: Vec<AnyElement> = Vec::new();
    if app.reco_on() {
        for (k, kind) in MixKind::ALL.iter().enumerate() {
            let kind = *kind;
            let n = app.reco.mix(kind).map_or(0, |m| m.tracks.len());
            buttons.push(
                list_button(
                    ("mix-pl", k),
                    kind.name(),
                    format!("Made for you · {}", plural(n as u64, "track")),
                    app.playlists.selected == PlaylistSel::Mix(kind),
                    false,
                    t,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_playlist(PlaylistSel::Mix(kind), cx)
                }))
                .into_any_element(),
            );
        }
        buttons.push(
            div()
                .h(px(1.))
                .my(px(6.))
                .mx(px(8.))
                .bg(t.border)
                .into_any_element(),
        );
    }
    for (k, s) in SmartPlaylist::ALL.iter().enumerate() {
        let s = *s;
        let sel = app.playlists.selected == PlaylistSel::Smart(s);
        buttons.push(
            list_button(
                ("smart-pl", k),
                s.name(),
                format!(
                    "Smart playlist · {}",
                    plural(app.playlists.smart_counts[k] as u64, "track")
                ),
                sel,
                true,
                t,
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.select_playlist(PlaylistSel::Smart(s), cx)),
            )
            .into_any_element(),
        );
    }
    if !app.library.playlists.is_empty() {
        buttons.push(
            div()
                .h(px(1.))
                .my(px(6.))
                .mx(px(8.))
                .bg(t.border)
                .into_any_element(),
        );
    }
    for p in &app.library.playlists {
        let id = p.id;
        let sel = app.playlists.selected == PlaylistSel::User(id);
        buttons.push(
            list_button(
                ("user-pl", id as usize),
                p.name.clone(),
                format!("Playlist · {}", plural(p.tracks, "track")),
                sel,
                false,
                t,
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.select_playlist(PlaylistSel::User(id), cx)),
            )
            .into_any_element(),
        );
    }

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(LIST_W))
        .h_full()
        .gap(px(20.))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(crate::widgets::page_title("Playlists", t))
                .child(
                    div()
                        .relative()
                        .child(
                            div()
                                .id("pl-add")
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(40.))
                                .rounded(px(10.))
                                .bg(if add_open {
                                    t.button_hover
                                } else {
                                    t.surface_2
                                })
                                .border_1()
                                .border_color(t.border_input)
                                .cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .tooltip(|_, cx| text_tooltip("New or import playlist", cx))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.open_popup(Popup::PlaylistAdd, cx)
                                }))
                                .child(icon(Icon::Plus, px(18.), t.text_2)),
                        )
                        .when(add_open, |d| {
                            d.child(popup::dropdown(
                                menu_panel(t)
                                    .child(
                                        menu_item("pl-new", "New playlist", None, true, t)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.popup = None;
                                                this.create_playlist(cx);
                                            })),
                                    )
                                    .child(
                                        menu_item("pl-import", "Import .m3u8…", None, true, t)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.popup = None;
                                                this.import_playlist(cx);
                                            })),
                                    ),
                            ))
                        }),
                ),
        )
        .child(
            div()
                .id("pl-list")
                .flex()
                .flex_col()
                .gap(px(4.))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(buttons),
        )
}

/// One 48px row: #, Title, Artist, Added, Time.
fn row(
    app: &LumenApp,
    k: usize,
    t: &Theme,
    draggable: bool,
    cx: &mut Context<LumenApp>,
) -> Stateful<Div> {
    let rows = app.playlists.rows.clone();
    let tracks = app.playlists.tracks.clone();
    let (ix, added, position) = rows[k];
    let c = &app.library.catalog;
    let track = c.track(ix);
    let id = track.id;
    let playing = app.playing == Some(id);
    let selected = app.songs.selected == Some(id);
    let hover = t.surface_hover;
    let accent_border = t.accent_border;
    let user = match app.playlists.selected {
        PlaylistSel::User(pid) => Some(pid),
        PlaylistSel::Smart(_) | PlaylistSel::Mix(_) => None,
    };
    let title: SharedString = track.title.clone().into();
    let cell = |w: Option<f32>, grow: f32| {
        let d = div().flex().items_center().min_w_0();
        match w {
            Some(w) => d.w(px(w)).flex_none(),
            None => {
                let mut d = d;
                let s = d.style();
                s.flex_grow = Some(grow);
                s.flex_shrink = Some(1.);
                s.flex_basis = Some(relative(0.).into());
                d
            }
        }
    };
    let text = |s: String, size: f32, color| {
        div()
            .min_w_0()
            .truncate()
            .text_size(px(size))
            .text_color(color)
            .child(s)
    };

    div()
        .id(("pl-row", k))
        .w_full()
        .flex()
        .flex_row()
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
                this.play_rows(&tracks, k, false, cx);
            }
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                window.focus(&this.table_focus);
                this.songs.selected = Some(id);
                this.popup = Some(Popup::Song {
                    track: id,
                    at: e.position,
                    submenu: false,
                    entry: user.map(|pid| (pid, position)),
                });
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .when(draggable, |d| {
            d.cursor_grab()
                .on_drag(
                    DraggedEntry {
                        position,
                        title: title.clone(),
                    },
                    |e: &DraggedEntry, _: Point<Pixels>, _, cx: &mut App| {
                        let title = e.title.clone();
                        cx.new(|_| DragPreview { title })
                    },
                )
                .drag_over::<DraggedEntry>(move |s, _, _, _| {
                    s.border_t_2().border_color(accent_border)
                })
                .on_drop(cx.listener(move |this, e: &DraggedEntry, _, cx| {
                    this.drop_entry(e.position, position, cx)
                }))
        })
        .child(cell(Some(48.), 0.).child(if playing {
            icon(Icon::AudioLines, px(16.), t.accent).into_any_element()
        } else {
            div()
                .tabular_nums()
                .text_size(px(13.))
                .text_color(t.text_muted)
                .child((k + 1).to_string())
                .into_any_element()
        }))
        .child(
            cell(None, 2.2).child(
                text(
                    track.title.clone(),
                    14.,
                    if playing { t.accent } else { t.text },
                )
                .font_weight(FontWeight::MEDIUM),
            ),
        )
        .child(cell(None, 1.4).child(text(track.artist.clone(), 13., t.text_2)))
        .child(cell(Some(110.), 0.).child(text(short_date(added), 13., t.text_3).tabular_nums()))
        .child(
            cell(Some(72.), 0.)
                .justify_end()
                .child(text(format_time(track.duration_ms / 1000), 13., t.text_3).tabular_nums()),
        )
}

/// "24 Nov" this year, "24 Nov 2025" otherwise.
fn short_date(ms: i64) -> String {
    let full = crate::ui::format_date(ms);
    let this_year = crate::ui::format_date(now_ms());
    match (full.rsplit_once(' '), this_year.rsplit_once(' ')) {
        (Some((dm, y)), Some((_, now_y))) if y == now_y => dm.to_string(),
        _ => full,
    }
}

fn header_cells(t: &Theme) -> impl IntoElement + use<> {
    let label =
        |s: &'static str| tracked_label(s, px(11.), 0.06, FontWeight::SEMIBOLD, t.text_muted);
    let fixed = |w: f32| div().flex().items_center().w(px(w)).flex_none();
    let grow = |g: f32| {
        let mut d = div().flex().items_center().min_w_0();
        let s = d.style();
        s.flex_grow = Some(g);
        s.flex_shrink = Some(1.);
        s.flex_basis = Some(relative(0.).into());
        d
    };
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(16.))
        .h(px(40.))
        .px(px(16.))
        .bg(t.surface)
        .border_b_1()
        .border_color(t.border)
        .child(fixed(48.).child(label("#")))
        .child(grow(2.2).child(label("Title")))
        .child(grow(1.4).child(label("Artist")))
        .child(fixed(110.).child(label("Added")))
        .child(fixed(72.).justify_end().child(label("Time")))
}

fn right_column(
    app: &mut LumenApp,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let l = app.settings.library.clone();
    let user = app.selected_user();
    let mix = match app.playlists.selected {
        PlaylistSel::Mix(k) => Some(k),
        _ => None,
    };
    let (kind, name) = match app.playlists.selected {
        PlaylistSel::Mix(k) => ("Made for you", k.name().to_string()),
        PlaylistSel::Smart(s) => ("Smart playlist", s.name().to_string()),
        PlaylistSel::User(_) => (
            "Playlist",
            user.as_ref().map(|u| u.1.clone()).unwrap_or_default(),
        ),
    };
    let n = app.playlists.rows.len();
    let sort_open = app.popup == Some(Popup::PlaylistSort);
    let menu_open = app.popup == Some(Popup::PlaylistMenu);
    let draggable = user.is_some() && l.playlist_sort == PlaylistSort::Custom;

    let title: AnyElement = match &app.playlists.rename {
        Some(input) => div()
            .display_font()
            .text_size(px(26.))
            .line_height(px(34.))
            .font_weight(FontWeight::BOLD)
            .text_color(t.text)
            .px(px(8.))
            .rounded(px(8.))
            .bg(t.surface_2)
            .border_1()
            .border_color(t.accent_border)
            .child(input.clone())
            .into_any_element(),
        None => div()
            .display_font()
            .text_size(px(26.))
            .line_height(px(34.))
            .font_weight(FontWeight::BOLD)
            .text_color(t.text)
            .truncate()
            .child(name.clone())
            .into_any_element(),
    };

    let controls = div()
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
                        "pl-sort",
                        Some("Sort by"),
                        l.playlist_sort.label(),
                        sort_open,
                        t,
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.open_popup(Popup::PlaylistSort, cx)),
                    ),
                )
                .when(sort_open, |d| {
                    d.child(popup::dropdown(popup::options_menu(
                        "pl-sort-opt",
                        PlaylistSort::ALL
                            .iter()
                            .map(|s| (s.label(), *s == l.playlist_sort)),
                        t,
                        cx,
                        |this, i, cx| {
                            this.popup = None;
                            let s = PlaylistSort::ALL[i];
                            let dir = if s == this.settings.library.playlist_sort {
                                this.settings.library.playlist_sort_dir
                            } else {
                                s.natural_dir()
                            };
                            this.set_playlist_sort(s, dir, cx);
                        },
                    )))
                }),
        )
        .child(
            secondary_button("pl-dir", None, l.playlist_sort_dir.button_label(), true, t).on_click(
                cx.listener(|this, _, _, cx| {
                    let l = &this.settings.library;
                    let (s, d) = (l.playlist_sort, l.playlist_sort_dir.flip());
                    this.set_playlist_sort(s, d, cx);
                }),
            ),
        )
        .when_some(mix, |d, kind| {
            d.child(
                secondary_button("pl-save-mix", None, "Save as playlist", n > 0, t).when(
                    n > 0,
                    |b| {
                        b.on_click(
                            cx.listener(move |this, _, _, cx| this.save_mix_as_playlist(kind, cx)),
                        )
                    },
                ),
            )
        })
        .child(
            accent_button("pl-play", Some(Icon::Play), "Play", n > 0, t).when(n > 0, |b| {
                b.on_click(cx.listener(|this, _, _, cx| {
                    let tracks = this.playlists.tracks.clone();
                    this.play_rows(&tracks, 0, false, cx);
                }))
            }),
        )
        .when(user.is_some(), |d| {
            let hover = t.button_hover;
            d.child(
                div()
                    .relative()
                    .child(
                        div()
                            .id("pl-more")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(38.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(t.border_input)
                            .bg(if menu_open {
                                t.button_hover
                            } else {
                                t.surface_2
                            })
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_popup(Popup::PlaylistMenu, cx)
                            }))
                            .child(icon(Icon::More, px(18.), t.text_2)),
                    )
                    .when(menu_open, |d| {
                        d.child(
                            div().absolute().right_0().child(popup::dropdown(
                                menu_panel(t)
                                    .child(
                                        menu_item("pl-rename", "Rename", None, true, t).on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.start_rename(window, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        menu_item("pl-export", "Export .m3u8…", None, true, t)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.popup = None;
                                                this.export_playlist(cx);
                                            })),
                                    )
                                    .child(menu_separator(t))
                                    .child(
                                        menu_item("pl-delete", "Delete playlist…", None, true, t)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.popup = None;
                                                this.playlists.confirm_delete = true;
                                                cx.notify();
                                            })),
                                    ),
                            )),
                        )
                    }),
            )
        });

    // The buttons move below the title when both don't fit (the mockup's
    // header wraps the same way).
    let header = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_end()
        .justify_between()
        .gap(px(16.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .flex_1()
                .flex_basis(px(260.))
                .min_w(px(260.))
                .child(crate::ui::section_label(
                    format!("{kind} · {}", plural(n as u64, "track")),
                    t.text_muted,
                ))
                .child(title),
        )
        .child(controls);

    let notice: Option<AnyElement> = if app.playlists.confirm_delete {
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .p(px(12.))
                .rounded(px(10.))
                .bg(t.surface_2)
                .border_1()
                .border_color(t.border_input)
                .child(
                    div()
                        .flex_1()
                        .text_size(px(13.))
                        .text_color(t.text)
                        .child(format!("Delete “{name}”? Its songs stay in your library.")),
                )
                .child(
                    accent_button("pl-delete-yes", None, "Delete", true, t)
                        .on_click(cx.listener(|this, _, _, cx| this.delete_selected_playlist(cx))),
                )
                .child(
                    secondary_button("pl-delete-no", None, "Cancel", true, t).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.playlists.confirm_delete = false;
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element(),
        )
    } else {
        app.playlists.message.clone().map(|m| {
            div()
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(t.text_muted)
                .child(m)
                .into_any_element()
        })
    };

    let hint = if user.is_some() && !draggable && n > 1 {
        Some("Pick “Custom order” to drag songs into a new order.")
    } else if draggable && n > 1 {
        Some("Drag songs to reorder.")
    } else {
        None
    };

    let body: AnyElement = if n == 0 {
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .text_size(px(14.))
            .text_color(t.text_muted)
            .child(match app.playlists.selected {
                PlaylistSel::User(_) => {
                    "This playlist is empty. Right-click a song and pick Add to playlist."
                }
                PlaylistSel::Smart(_) => "No songs match this smart playlist yet.",
                PlaylistSel::Mix(kind) => kind.empty_hint(),
            })
            .into_any_element()
    } else {
        let list = uniform_list(
            "pl-rows",
            n,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let t = Theme::global(cx).clone();
                range
                    .map(|k| row(this, k, &t, draggable, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .track_scroll(app.playlists.scroll.clone());
        focusable_table(
            div().id("pl-table").flex().flex_col().size_full(),
            &app.table_focus,
            cx,
        )
        .child(header_cells(t))
        .child(list)
        .into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .gap(px(16.))
        .child(header)
        .children(notice)
        .when_some(hint, |d, h| {
            d.child(div().text_size(px(12.)).text_color(t.text_muted).child(h))
        })
        .child(table_frame(t).flex_1().min_h_0().child(body))
}

pub fn render(
    app: &mut LumenApp,
    t: &Theme,
    _window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_row()
        .gap(px(32.))
        .size_full()
        .pt(px(32.))
        .pb(px(24.))
        .px(px(40.))
        .bg(t.bg)
        .child(left_column(app, t, cx))
        .child(right_column(app, t, cx))
}
