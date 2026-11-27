//! Settings screen: the Audio output, Library and Lyrics cards. Every
//! setting is saved to `settings.toml` as soon as it changes.

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::app::{LumenApp, Popup};
use crate::assets::Icon;
use crate::popup;
use crate::theme::Theme;
use crate::ui::{FontExt, format_count, icon, section_label};
use crate::widgets::select;
use lumen_core::{AudioSettings, LyricsFont, SongSort, SortDir};

pub fn render(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    div()
        .id("settings")
        .flex()
        .flex_col()
        .gap(px(28.))
        .size_full()
        .py(px(32.))
        .px(px(40.))
        .bg(t.bg)
        .overflow_y_scroll()
        .child(
            div()
                .display_font()
                .text_size(px(32.))
                .line_height(px(40.))
                .font_weight(FontWeight::BOLD)
                .text_color(t.text)
                .child("Settings"),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(20.))
                .max_w(px(1100.))
                .items_start()
                .child(audio_card(app, t, cx))
                .child(library_card(app, t, cx))
                .child(lyrics_card(app, t, cx)),
        )
}

fn library_card(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    let lib = &app.library;
    let scanning = lib.scan.is_some();
    let can_rescan = !lib.folders.is_empty() && !scanning && lib.open_error.is_none();

    let mut rows: Vec<AnyElement> = Vec::new();
    for f in &lib.folders {
        rows.push(folder_row(f, t, cx).into_any_element());
    }
    let list = if rows.is_empty() {
        div()
            .px(px(12.))
            .py(px(14.))
            .rounded(px(10.))
            .bg(t.surface_2)
            .text_size(px(13.))
            .line_height(px(19.))
            .text_color(t.text_muted)
            .child("No music folders yet. Add the folder that holds your music; Lumen reads it in place and never changes your files.")
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(rows)
            .into_any_element()
    };

    let status: Option<SharedString> = if let Some(e) = &lib.open_error {
        Some(format!("The library database could not be opened: {e}").into())
    } else if let Some((done, total)) = lib.scan {
        Some(if total == 0 {
            "Scanning… looking for music files".into()
        } else {
            format!(
                "Scanning… {} / {}",
                format_count(done as u64),
                format_count(total as u64)
            )
            .into()
        })
    } else {
        lib.last_error.clone().map(SharedString::from)
    };

    div()
        .flex()
        .flex_col()
        .gap(px(16.))
        .flex_1()
        .min_w(px(380.))
        .max_w(px(560.))
        .p(px(20.))
        .rounded(px(14.))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .display_font()
                .text_size(px(18.))
                .line_height(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .child("Library"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(section_label("Music folders", t.text_muted))
                .child(list),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(10.))
                .child(
                    button(
                        "add-folder",
                        Icon::FolderPlus,
                        "Add folder",
                        lib.open_error.is_none(),
                        t,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                )
                .child(
                    button("rescan", Icon::Refresh, "Rescan now", can_rescan, t)
                        .when(can_rescan, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.rescan(cx)))
                        }),
                ),
        )
        .when_some(status, |d, s| {
            d.child(
                div()
                    .tabular_nums()
                    .text_size(px(12.))
                    .line_height(px(17.))
                    .text_color(t.text_muted)
                    .child(s),
            )
        })
        .child(default_sort(app, t, cx))
        .child(
            toggle(
                "watch-toggle",
                "Watch folders for changes",
                "Pick up added, edited and removed files while Lumen runs",
                app.settings.library.watch,
                t,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_library_settings(|l| l.watch = !l.watch, cx)
            })),
        )
        .child(
            toggle(
                "mtime-toggle",
                "Track file date modified",
                "Off: Date modified keeps the date a file was first seen, so retagging doesn't reorder songs",
                app.settings.library.track_mtime,
                t,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_library_settings(|l| l.track_mtime = !l.track_mtime, cx)
            })),
        )
        .child(reset_play_counts(app, t, cx))
}

/// "Reset play counts" with a confirm step.
fn reset_play_counts(
    app: &LumenApp,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    if !app.confirm_reset {
        return div()
            .flex()
            .child(
                crate::widgets::secondary_button("reset-plays", None, "Reset play counts", true, t)
                    .bg(gpui::transparent_black())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_reset = true;
                        cx.notify();
                    })),
            )
            .into_any_element();
    }
    let plays: u64 = app
        .library
        .catalog
        .tracks
        .iter()
        .map(|t| u64::from(t.track.play_count))
        .sum();
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(14.))
        .rounded(px(10.))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border_input)
        .child(
            div()
                .text_size(px(13.))
                .line_height(px(19.))
                .text_color(t.text)
                .child(format!(
                    "Reset all play counts to zero? This clears {} and \"last played\", and can't be undone.",
                    crate::ui::plural(plays, "play")
                )),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(10.))
                .child(
                    crate::widgets::accent_button("reset-confirm", None, "Reset play counts", true, t)
                        .on_click(cx.listener(|this, _, _, cx| this.reset_play_counts(cx))),
                )
                .child(
                    crate::widgets::secondary_button("reset-cancel", None, "Cancel", true, t)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_reset = false;
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
}

/// Audio output: device, sample rate, exclusive mode, gapless, ReplayGain.
fn audio_card(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    let a = app.settings.audio.clone();
    let devices = app.player.read(cx).devices.clone();
    let device_open = app.popup == Some(Popup::SettingsDevice);
    let rate_open = app.popup == Some(Popup::SettingsRate);
    let device_label: String = match &a.device_id {
        None => "System default".into(),
        Some(id) => devices
            .iter()
            .find(|d| &d.id == id)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| {
                format!(
                    "{} (not connected)",
                    a.device_name
                        .clone()
                        .unwrap_or_else(|| "Saved device".into())
                )
            }),
    };
    // "System default" first, then every device.
    let mut options: Vec<(String, Option<(String, String)>)> = vec![(
        match devices.iter().find(|d| d.is_default) {
            Some(d) => format!("System default ({})", d.name),
            None => "System default".into(),
        },
        None,
    )];
    for d in &devices {
        options.push((d.name.clone(), Some((d.id.clone(), d.name.clone()))));
    }
    let checked: Vec<bool> = options
        .iter()
        .map(|(_, v)| v.as_ref().map(|(id, _)| id.clone()) == a.device_id)
        .collect();
    let picks = options.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>();

    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .flex_1()
        .min_w(px(380.))
        .max_w(px(560.))
        .p(px(20.))
        .rounded(px(14.))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .display_font()
                .text_size(px(18.))
                .line_height(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .child("Audio output"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(section_label("Output device", t.text_muted))
                .child(
                    div()
                        .relative()
                        .child(
                            select("audio-device", None, device_label, device_open, t)
                                .w_full()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.player.read(cx).refresh_devices();
                                    this.open_popup(Popup::SettingsDevice, cx)
                                })),
                        )
                        .when(device_open, |d| {
                            d.child(popup::dropdown(popup::options_menu(
                                "audio-device-opt",
                                options
                                    .iter()
                                    .zip(&checked)
                                    .map(|((l, _), c)| (l.as_str(), *c)),
                                t,
                                cx,
                                move |this, i, cx| {
                                    this.popup = None;
                                    let pick = picks.get(i).cloned().flatten();
                                    this.update_audio_settings(
                                        |a| match pick {
                                            Some((id, name)) => {
                                                a.device_id = Some(id);
                                                a.device_name = Some(name);
                                            }
                                            None => {
                                                a.device_id = None;
                                                a.device_name = None;
                                            }
                                        },
                                        cx,
                                    );
                                },
                            )))
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(section_label("Sample rate", t.text_muted))
                .child(
                    div()
                        .relative()
                        .child(
                            select(
                                "audio-rate",
                                None,
                                AudioSettings::rate_label(a.sample_rate),
                                rate_open,
                                t,
                            )
                            .w_full()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_popup(Popup::SettingsRate, cx)
                            })),
                        )
                        .when(rate_open, |d| {
                            d.child(popup::dropdown(popup::options_menu(
                                "audio-rate-opt",
                                AudioSettings::RATES
                                    .iter()
                                    .map(|r| (AudioSettings::rate_label(*r), *r == a.sample_rate)),
                                t,
                                cx,
                                |this, i, cx| {
                                    this.popup = None;
                                    this.update_audio_settings(
                                        |a| a.sample_rate = AudioSettings::RATES[i],
                                        cx,
                                    );
                                },
                            )))
                        })
                        .when(a.sample_rate.is_some(), |d| {
                            d.child(
                                div()
                                    .mt(px(6.))
                                    .text_size(px(12.))
                                    .line_height(px(17.))
                                    .text_color(t.text_muted)
                                    .child(
                                        "Exclusive mode resamples every file to this rate \
                                         (shown in the output badge). Shared mode uses the \
                                         Windows mixer rate.",
                                    ),
                            )
                        }),
                ),
        )
        .child(
            toggle(
                "exclusive-toggle",
                "WASAPI exclusive mode",
                "Bypasses the Windows mixer for bit-perfect output",
                a.exclusive,
                t,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_audio_settings(|a| a.exclusive = !a.exclusive, cx)
            })),
        )
        .child(
            toggle(
                "gapless-toggle",
                "Gapless playback",
                "No silence between album tracks",
                a.gapless,
                t,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_audio_settings(|a| a.gapless = !a.gapless, cx)
            })),
        )
        .child(
            toggle(
                "replaygain-toggle",
                "ReplayGain",
                "Normalise loudness using file tags",
                a.replaygain,
                t,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_audio_settings(|a| a.replaygain = !a.replaygain, cx)
            })),
        )
}

/// Lyrics: sources, word highlight and the lyrics font.
fn lyrics_card(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    let l = app.settings.lyrics.clone();
    let font_open = app.popup == Some(Popup::SettingsLyricsFont);
    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .flex_1()
        .min_w(px(380.))
        .max_w(px(560.))
        .p(px(20.))
        .rounded(px(14.))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .display_font()
                .text_size(px(18.))
                .line_height(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .child("Lyrics"),
        )
        .child(
            toggle(
                "lrc-toggle",
                "Load .lrc next to audio file",
                "Same file name, same folder",
                l.load_lrc,
                t,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_lyrics_settings(|l| l.load_lrc = !l.load_lrc, cx)
            })),
        )
        .child(
            toggle(
                "embedded-toggle",
                "Use embedded lyrics tags",
                "SYLT / LYRICS tags in FLAC and MP3",
                l.embedded,
                t,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_lyrics_settings(|l| l.embedded = !l.embedded, cx)
            })),
        )
        .child(
            toggle(
                "words-toggle",
                "Word-by-word highlight",
                "For enhanced .lrc files with word timings",
                l.word_highlight,
                t,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_lyrics_settings(|l| l.word_highlight = !l.word_highlight, cx)
            })),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(section_label("Lyrics font", t.text_muted))
                .child(
                    div()
                        .relative()
                        .child(
                            select("lyrics-font", None, l.font.label(), font_open, t).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.open_popup(Popup::SettingsLyricsFont, cx)
                                }),
                            ),
                        )
                        .when(font_open, |d| {
                            d.child(popup::dropdown(popup::options_menu(
                                "lyrics-font-opt",
                                LyricsFont::ALL.iter().map(|f| (f.label(), *f == l.font)),
                                t,
                                cx,
                                |this, i, cx| {
                                    this.popup = None;
                                    this.update_lyrics_settings(
                                        |l| l.font = LyricsFont::ALL[i],
                                        cx,
                                    );
                                },
                            )))
                        }),
                ),
        )
}

/// A settings toggle: label (500) over a 12px muted hint, and a 48×28 pill
/// switch (`accent` on, `track` off) with a 22px knob.
fn toggle(
    id: &'static str,
    label: &'static str,
    hint: &'static str,
    on: bool,
    t: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(16.))
        .cursor_pointer()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.text)
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(t.text_muted)
                        .child(hint),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .w(px(48.))
                .h(px(28.))
                .px(px(3.))
                .rounded_full()
                .bg(if on { t.accent } else { t.track })
                .when(on, |d| d.justify_end())
                .child(div().size(px(22.)).rounded_full().bg(t.switch_knob)),
        )
}

/// "Default song sort": the Songs screen's sort, which persists either way.
fn default_sort(app: &LumenApp, t: &Theme, cx: &mut Context<LumenApp>) -> impl IntoElement + use<> {
    let l = app.settings.library.clone();
    let sort_open = app.popup == Some(Popup::SettingsSongSort);
    let dir_open = app.popup == Some(Popup::SettingsSongDir);
    let dir_label = |d: SortDir| match d {
        SortDir::Asc => "Ascending",
        SortDir::Desc => "Descending",
    };
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(section_label("Default song sort", t.text_muted))
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(10.))
                .child(
                    div()
                        .relative()
                        .child(
                            select("settings-sort", None, l.song_sort.label(), sort_open, t)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.open_popup(Popup::SettingsSongSort, cx)
                                })),
                        )
                        .when(sort_open, |d| {
                            d.child(popup::dropdown(popup::options_menu(
                                "settings-sort-opt",
                                SongSort::ALL.iter().map(|s| (s.label(), *s == l.song_sort)),
                                t,
                                cx,
                                |this, i, cx| {
                                    this.popup = None;
                                    let dir = this.settings.library.song_sort_dir;
                                    this.set_song_sort(SongSort::ALL[i], dir, cx);
                                },
                            )))
                        }),
                )
                .child(
                    div()
                        .relative()
                        .child(
                            select(
                                "settings-dir",
                                None,
                                dir_label(l.song_sort_dir),
                                dir_open,
                                t,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_popup(Popup::SettingsSongDir, cx)
                            })),
                        )
                        .when(dir_open, |d| {
                            d.child(popup::dropdown(popup::options_menu(
                                "settings-dir-opt",
                                [SortDir::Asc, SortDir::Desc]
                                    .into_iter()
                                    .map(|d| (dir_label(d), d == l.song_sort_dir)),
                                t,
                                cx,
                                |this, i, cx| {
                                    this.popup = None;
                                    let sort = this.settings.library.song_sort;
                                    let dir = if i == 0 { SortDir::Asc } else { SortDir::Desc };
                                    this.set_song_sort(sort, dir, cx);
                                },
                            )))
                        }),
                ),
        )
}

fn folder_row(
    f: &lumen_library::FolderInfo,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let id = f.id;
    let group: SharedString = format!("folder-{id}").into();
    let hover_bg = t.surface_hover;
    let btn_hover = t.button_hover;
    let count = if f.tracks == 1 {
        "1 track".to_string()
    } else {
        format!("{} tracks", format_count(f.tracks))
    };
    div()
        .id(SharedString::from(format!("folder-row-{id}")))
        .group(group.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.))
        .h(px(44.))
        .px(px(12.))
        .rounded(px(8.))
        .hover(move |s| s.bg(hover_bg))
        .child(icon(Icon::Folder, px(16.), t.text_3))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .text_color(t.text)
                        .truncate()
                        .child(f.path.to_string_lossy().into_owned()),
                )
                .when(!f.available, |d| {
                    d.child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(t.text_muted)
                            .child("Not available right now; its tracks are kept"),
                    )
                }),
        )
        .child(
            div()
                .flex_none()
                .tabular_nums()
                .text_size(px(12.))
                .text_color(t.text_muted)
                .child(count),
        )
        .child(
            div()
                .id(SharedString::from(format!("folder-remove-{id}")))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded(px(6.))
                .cursor_pointer()
                .invisible()
                .group_hover(group, |s| s.visible())
                .hover(move |s| s.bg(btn_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.remove_folder(id, cx)))
                .child(icon(Icon::Close, px(14.), t.text_3)),
        )
}

/// Secondary button: 38px, `surface_2`, `border_input`.
fn button(
    id: &'static str,
    glyph: Icon,
    label: &'static str,
    enabled: bool,
    t: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let hover_bg = t.button_hover;
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .h(px(38.))
        .px(px(14.))
        .rounded(px(8.))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border_input)
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.text)
        .when(enabled, |b| {
            b.cursor_pointer().hover(move |s| s.bg(hover_bg))
        })
        .when(!enabled, |b| b.opacity(0.45))
        .child(icon(glyph, px(16.), t.text_2))
        .child(label)
}
