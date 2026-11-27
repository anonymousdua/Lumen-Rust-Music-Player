//! Popups: select dropdowns, the song context menu and the lyrics offset
//! editor. A transparent backdrop under them closes them on any click.

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, Point, SharedString, StatefulInteractiveElement, Styled, Window, anchored, deferred,
    div, px, relative,
};
use lumen_core::TrackId;

use crate::app::{LumenApp, Popup};
use crate::assets::Icon;
use crate::theme::Theme;
use crate::ui::{FontExt, icon};
use crate::widgets::{menu_item, menu_panel, menu_separator, secondary_button};

/// Places `menu` just below the element it is a child of (that element must
/// be `relative()`), drawn above everything else and kept inside the window.
pub fn dropdown(menu: impl IntoElement) -> impl IntoElement {
    div()
        .absolute()
        .top(px(42.))
        .left_0()
        .child(deferred(anchored().snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2))
}

/// A list of options with a check mark on the current one.
pub fn options_menu<'a>(
    id: &'static str,
    items: impl Iterator<Item = (&'a str, bool)>,
    t: &Theme,
    cx: &mut Context<LumenApp>,
    on_pick: impl Fn(&mut LumenApp, usize, &mut Context<LumenApp>) + Clone + 'static,
) -> gpui::Div {
    let mut rows = Vec::new();
    for (i, (label, checked)) in items.enumerate() {
        let pick = on_pick.clone();
        rows.push(
            menu_item((id, i), label.to_string(), Some(checked), true, t)
                .on_click(cx.listener(move |this, _, _, cx| pick(this, i, cx))),
        );
    }
    menu_panel(t).occlude().children(rows)
}

/// Backdrop plus whichever window-positioned popup is open.
pub fn render(
    app: &LumenApp,
    t: &Theme,
    window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> Vec<AnyElement> {
    let Some(popup) = app.popup.clone() else {
        return Vec::new();
    };
    let backdrop = deferred(
        div()
            .id("popup-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(MouseButton::Left, close_listener(cx))
            .on_mouse_down(MouseButton::Right, close_listener(cx))
            .on_mouse_down(MouseButton::Middle, close_listener(cx))
            .on_scroll_wheel(cx.listener(|this, _, _, cx| this.close_popup(cx))),
    )
    .with_priority(1)
    .into_any_element();

    let mut out = vec![backdrop];
    match popup {
        Popup::Song {
            track,
            at,
            submenu,
            entry,
        } => {
            out.push(positioned(
                at,
                song_menu(
                    app,
                    SongMenuAt {
                        id: track,
                        at,
                        submenu,
                        entry,
                    },
                    t,
                    window,
                    cx,
                ),
            ));
        }
        Popup::LyricsOffset {
            track,
            at,
            offset_ms,
        } => {
            out.push(positioned(at, offset_editor(app, track, offset_ms, t, cx)));
        }
        Popup::Album { id, at } => out.push(positioned(at, album_menu(app, id, t, cx))),
        Popup::Artist { id, at } => out.push(positioned(at, artist_menu(app, id, t, cx))),
        _ => {}
    }
    out
}

fn close_listener(
    cx: &mut Context<LumenApp>,
) -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static + use<> {
    let this = cx.entity().downgrade();
    move |_, _, cx| {
        let _ = this.update(cx, |a, cx| a.close_popup(cx));
    }
}

/// Hovering any other row of the song menu closes the playlist submenu.
fn hover_off_listener(
    cx: &mut Context<LumenApp>,
) -> impl Fn(&bool, &mut Window, &mut gpui::App) + 'static + use<> {
    let this = cx.entity().downgrade();
    move |hovered, _, cx| {
        if !*hovered {
            return;
        }
        let _ = this.update(cx, |a, cx| {
            if let Some(Popup::Song { submenu, .. }) = &mut a.popup
                && *submenu
            {
                *submenu = false;
                cx.notify();
            }
        });
    }
}

fn positioned(at: Point<Pixels>, child: impl IntoElement) -> AnyElement {
    deferred(
        anchored()
            .position(at)
            .snap_to_window_with_margin(px(8.))
            .child(child),
    )
    .with_priority(2)
    .into_any_element()
}

/// Where the song menu was opened.
struct SongMenuAt {
    id: TrackId,
    at: Point<Pixels>,
    submenu: bool,
    /// (playlist, position) when opened inside a user playlist.
    entry: Option<(i64, usize)>,
}

fn song_menu(
    app: &LumenApp,
    m: SongMenuAt,
    t: &Theme,
    window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let SongMenuAt {
        id,
        at,
        submenu,
        entry,
    } = m;
    let c = app.library.catalog.clone();
    let Some(ix) = c.index_of(id) else {
        return menu_panel(t).into_any_element();
    };
    let track = c.track(ix).clone();

    // Submenu: user playlists, then "New playlist".
    let mut lists: Vec<AnyElement> = Vec::new();
    for (i, p) in app.library.playlists.iter().enumerate() {
        let pid = p.id;
        lists.push(
            menu_item(("pl", i), p.name.clone(), None, true, t)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(l) = this.library.library() {
                        l.add_to_playlist(pid, vec![id]);
                    }
                    this.close_popup(cx);
                }))
                .into_any_element(),
        );
    }
    if !lists.is_empty() {
        lists.push(menu_separator(t));
    }
    lists.push(
        menu_item("pl-new", "New playlist", None, true, t)
            .on_click(cx.listener(move |this, _, _, cx| {
                let name = new_playlist_name(this);
                if let Some(l) = this.library.library() {
                    l.new_playlist(name, vec![id]);
                }
                this.close_popup(cx);
            }))
            .into_any_element(),
    );
    // Open the submenu to the left when there is no room on the right.
    let room_right = window.viewport_size().width - at.x > px(460.);

    let playlist_row = menu_item("ctx-playlist", "Add to playlist", None, true, t)
        .relative()
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if *hovered
                && let Some(Popup::Song { submenu, .. }) = &mut this.popup
                && !*submenu
            {
                *submenu = true;
                cx.notify();
            }
        }))
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(Popup::Song { submenu, .. }) = &mut this.popup {
                *submenu = !*submenu;
                cx.notify();
            }
        }))
        .child(icon(Icon::ChevronRight, px(14.), t.text_3))
        .when(submenu, |d| {
            d.child(
                div()
                    .absolute()
                    .top(px(-5.))
                    .map(|d| {
                        if room_right {
                            d.left(relative(1.)).pl(px(8.))
                        } else {
                            d.right(relative(1.)).pr(px(8.))
                        }
                    })
                    .child(menu_panel(t).occlude().children(lists)),
            )
        });

    let path = track.path.clone();
    let offset = track.lyrics_offset_ms;
    menu_panel(t)
        .occlude()
        .child(
            menu_item("ctx-next", "Play next", None, true, t)
                .on_hover(hover_off_listener(cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.enqueue(id, true, cx);
                    this.close_popup(cx);
                })),
        )
        .child(
            menu_item("ctx-queue", "Add to queue", None, true, t)
                .on_hover(hover_off_listener(cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.enqueue(id, false, cx);
                    this.close_popup(cx);
                })),
        )
        .when(app.reco_on(), |d| {
            d.child(
                menu_item("ctx-similar", "Play similar", None, true, t)
                    .on_hover(hover_off_listener(cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.play_similar(vec![id], id, cx);
                    })),
            )
        })
        .child(playlist_row)
        .when_some(entry, |d, (pid, pos)| {
            d.child(
                menu_item("ctx-remove", "Remove from playlist", None, true, t)
                    .on_hover(hover_off_listener(cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(l) = this.library.library() {
                            l.remove_from_playlist(pid, pos);
                        }
                        this.close_popup(cx);
                    })),
            )
        })
        .child(menu_separator(t))
        .child(
            menu_item("ctx-explorer", "Show in Explorer", None, true, t)
                .on_hover(hover_off_listener(cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.reveal_path(&path);
                    this.close_popup(cx);
                })),
        )
        .child(
            menu_item("ctx-offset", "Edit lyrics offset…", None, true, t)
                .on_hover(hover_off_listener(cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.popup = Some(Popup::LyricsOffset {
                        track: id,
                        at,
                        offset_ms: offset,
                    });
                    cx.notify();
                })),
        )
        .into_any_element()
}

/// Right-click menu on an album card: Play, Play similar.
fn album_menu(app: &LumenApp, id: i64, t: &Theme, cx: &mut Context<LumenApp>) -> AnyElement {
    menu_panel(t)
        .occlude()
        .child(
            menu_item("album-play", "Play", None, true, t).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.close_popup(cx);
                    if let Some(ix) = this.album_index(id) {
                        let list = this.library.catalog.albums[ix].tracks.clone();
                        this.play_rows(&list, 0, false, cx);
                    }
                },
            )),
        )
        .when(app.reco_on(), |d| {
            d.child(
                menu_item("album-similar", "Play similar", None, true, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.play_similar_album(id, cx))),
            )
        })
        .into_any_element()
}

/// Right-click menu on an artist card: Play, Play similar.
fn artist_menu(app: &LumenApp, id: i64, t: &Theme, cx: &mut Context<LumenApp>) -> AnyElement {
    menu_panel(t)
        .occlude()
        .child(
            menu_item("artist-play", "Play", None, true, t).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.close_popup(cx);
                    if let Some(ix) = this.artist_index(id) {
                        let list = this.library.catalog.artists[ix].tracks.clone();
                        this.play_rows(&list, 0, false, cx);
                    }
                },
            )),
        )
        .when(app.reco_on(), |d| {
            d.child(
                menu_item("artist-similar", "Play similar", None, true, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.play_similar_artist(id, cx))),
            )
        })
        .into_any_element()
}

/// "New playlist", or "New playlist 2" when that name is taken.
pub fn new_playlist_name(app: &LumenApp) -> String {
    let taken = |n: &str| app.library.playlists.iter().any(|p| p.name == n);
    let mut name = "New playlist".to_string();
    let mut i = 2;
    while taken(&name) {
        name = format!("New playlist {i}");
        i += 1;
    }
    name
}

/// "+0.5 s", "−1.0 s", "0.0 s".
pub fn format_offset(ms: i32) -> String {
    let s = ms as f32 / 1000.0;
    if ms > 0 {
        format!("+{s:.1} s")
    } else if ms < 0 {
        format!("−{:.1} s", -s)
    } else {
        "0.0 s".into()
    }
}

fn offset_editor(
    app: &LumenApp,
    id: TrackId,
    offset_ms: i32,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let c = &app.library.catalog;
    let title: SharedString = c
        .index_of(id)
        .map(|i| c.track(i).title.clone())
        .unwrap_or_default()
        .into();
    let step = |delta: i32| {
        move |this: &mut LumenApp, cx: &mut Context<LumenApp>| {
            if let Some(Popup::LyricsOffset { offset_ms, .. }) = &mut this.popup {
                *offset_ms = if delta == 0 {
                    0
                } else {
                    (*offset_ms + delta).clamp(-60_000, 60_000)
                };
                let v = *offset_ms;
                if let Some(l) = this.library.library() {
                    l.set_lyrics_offset(id, v);
                }
                cx.notify();
            }
        }
    };
    let (minus, plus, reset) = (step(-500), step(500), step(0));
    div()
        .w(px(300.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(16.))
        .rounded(px(12.))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border_input)
        .shadow_lg()
        .occlude()
        .ui_font()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .child("Lyrics offset"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(t.text_muted)
                        .truncate()
                        .child(title),
                ),
        )
        .child(
            div()
                .display_font()
                .tabular_nums()
                .text_size(px(28.))
                .font_weight(FontWeight::BOLD)
                .text_color(if offset_ms == 0 { t.text_2 } else { t.accent })
                .child(format_offset(offset_ms)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(8.))
                .child(
                    secondary_button("off-minus", None, "−0.5s", true, t)
                        .on_click(cx.listener(move |this, _, _, cx| minus(this, cx))),
                )
                .child(
                    secondary_button("off-plus", None, "+0.5s", true, t)
                        .on_click(cx.listener(move |this, _, _, cx| plus(this, cx))),
                )
                .child(
                    secondary_button("off-reset", None, "Reset", offset_ms != 0, t)
                        .when(offset_ms != 0, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| reset(this, cx)))
                        }),
                ),
        )
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(t.text_muted)
                .child("Saved for this track. Now Playing uses it to shift the lyrics."),
        )
}
