//! Now Playing: cover, title, artist, album and year, the format and output
//! badges, Up Next, and the lyrics panel (`lyrics_view.rs`). The transport
//! bar below it is a player bar in Transport mode.

use gpui::prelude::FluentBuilder;
use gpui::{
    Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use lumen_audio::OutputMode;
use lumen_core::Track;

use crate::app::LumenApp;
use crate::cover::{cover, placeholder};
use crate::theme::Theme;
use crate::ui::{FontExt, format_time, section_label};
use crate::widgets::text_tooltip;

/// A pill badge: accent (format) or outline (output, lyrics).
fn badge(label: impl Into<SharedString>, accent: bool, t: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(26.))
        .px(px(10.))
        .rounded_full()
        .border_1()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .tabular_nums()
        .map(|d| {
            if accent {
                d.bg(t.accent_bg_2)
                    .border_color(t.accent_border)
                    .text_color(t.accent)
            } else {
                d.border_color(t.border_soft).text_color(t.text_2)
            }
        })
        .child(label.into())
}

/// The badges for a playing track, in spec order.
fn badges(
    app: &LumenApp,
    track: &Track,
    t: &Theme,
    cx: &mut Context<LumenApp>,
) -> Vec<gpui::AnyElement> {
    let p = app.player.read(cx);
    let mut out = Vec::new();
    // Format: from the decoder (real bit depth and rate), else the tags.
    let format = match &p.info {
        Some(info) => info.format_badge(),
        None => match (track.bit_depth, track.sample_rate) {
            (Some(b), Some(r)) if track.is_lossless() => {
                format!("{} {b}-bit / {}", track.codec, lumen_audio::format_khz(r))
            }
            (_, Some(r)) => format!("{} / {}", track.codec, lumen_audio::format_khz(r)),
            _ => track.codec.clone(),
        },
    };
    out.push(badge(format, true, t).into_any_element());

    // Output: only known once the device is open.
    if let Some(s) = &p.signal {
        let mut why = vec![format!("{} · {}", s.device_name, s.output)];
        if let Some(reason) = &s.exclusive_fallback {
            why.push(format!("Exclusive mode unavailable: {reason}"));
        }
        if s.bit_perfect {
            why.push("Bit-perfect: samples reach the device unchanged.".into());
        } else if s.mode == OutputMode::Exclusive && s.volume_attenuates {
            why.push("Volume below 100% changes the samples.".into());
        }
        let tip = why.join("\n");
        out.push(
            badge(s.mode_badge(), false, t)
                .id("output-badge")
                .tooltip(move |_, cx| text_tooltip(tip.clone(), cx))
                .into_any_element(),
        );
        if let Some(db) = s.replaygain_db {
            out.push(badge(format!("ReplayGain {db:+.1} dB"), false, t).into_any_element());
        }
    }

    // From the lyrics actually loaded (the library flags can lag a new .lrc).
    match p.lyrics.as_deref() {
        Some(l) if l.synced && l.source == Some(lumen_lyrics::Source::LrcFile) => {
            let label = if l.has_words() {
                ".lrc synced · words"
            } else {
                ".lrc synced"
            };
            out.push(badge(label, false, t).into_any_element());
        }
        Some(l) if l.synced => out.push(badge("Synced lyrics (tag)", false, t).into_any_element()),
        _ if p.lyrics.is_none() && track.lyrics.synced() => {
            out.push(badge(".lrc synced", false, t).into_any_element())
        }
        _ => {}
    }
    out
}

pub fn render(
    app: &mut LumenApp,
    t: &Theme,
    _window: &mut Window,
    cx: &mut Context<LumenApp>,
) -> impl IntoElement + use<> {
    let thumbs = app.library.paths.thumbs.clone();
    let (track, upcoming) = {
        let p = app.player.read(cx);
        let up: Vec<(usize, Track)> = p
            .upcoming(3)
            .into_iter()
            .filter_map(|i| p.item(i).map(|tr| (i, tr.clone())))
            .collect();
        (p.track().cloned(), up)
    };

    let art = match &track {
        Some(tr) => cover(
            tr.cover_hash.as_deref(),
            &thumbs,
            &tr.album,
            px(240.),
            px(14.),
            t,
        ),
        None => placeholder("", px(240.), px(14.), t).into_any_element(),
    };
    let link = t.accent_hover;

    let left = div()
        .id("np-left")
        .flex()
        .flex_col()
        .flex_none()
        .w(px(300.))
        .h_full()
        .overflow_y_scroll()
        .child(art)
        .child(match &track {
            Some(tr) => {
                let artist_id = app
                    .library
                    .catalog
                    .artists
                    .iter()
                    .find(|a| a.name == tr.artist)
                    .map(|a| a.id);
                let album_line = match tr.year {
                    Some(y) => format!("{} · {y}", tr.album),
                    None => tr.album.clone(),
                };
                div()
                    .flex()
                    .flex_col()
                    .mt(px(24.))
                    .child(
                        div()
                            .display_font()
                            .text_size(px(28.))
                            .line_height(px(34.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.text)
                            .child(SharedString::from(tr.title.clone())),
                    )
                    .child(
                        div()
                            .id("np-artist")
                            .mt(px(6.))
                            .text_size(px(15.))
                            .text_color(t.text_2)
                            .when_some(artist_id, |d, id| {
                                d.cursor_pointer()
                                    .hover(move |s| s.text_color(link))
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.open_artist(id, cx)),
                                    )
                            })
                            .child(SharedString::from(tr.artist.clone())),
                    )
                    .child(
                        div()
                            .mt(px(2.))
                            .text_size(px(13.))
                            .text_color(t.text_muted)
                            .child(album_line),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(8.))
                            .mt(px(16.))
                            .children(badges(app, tr, t, cx)),
                    )
                    .into_any_element()
            }
            None => div()
                .flex()
                .flex_col()
                .mt(px(24.))
                .gap(px(6.))
                .child(
                    div()
                        .display_font()
                        .text_size(px(28.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(t.text_2)
                        .child("Nothing playing"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(t.text_muted)
                        .child("Double-click a song in Songs, Albums or Artists."),
                )
                .into_any_element(),
        })
        .when(!upcoming.is_empty(), |d| {
            let hover = t.surface_hover;
            let mut rows = Vec::new();
            for (i, tr) in upcoming {
                rows.push(
                    div()
                        .id(("up-next", i))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(44.))
                        .px(px(8.))
                        .rounded(px(8.))
                        .border_b_1()
                        .border_color(t.border_row)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.player.update(cx, |p, _| p.play_index(i));
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(14.))
                                .child({
                                    let text = format!("{} · {}", tr.title, tr.artist);
                                    let muted = tr.title.len()..text.len();
                                    gpui::StyledText::new(text).with_highlights([(
                                        muted,
                                        gpui::HighlightStyle {
                                            color: Some(t.text_muted),
                                            ..Default::default()
                                        },
                                    )])
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .tabular_nums()
                                .text_size(px(13.))
                                .text_color(t.text_3)
                                .child(format_time(tr.duration_ms / 1000)),
                        ),
                );
            }
            d.child(
                div()
                    .mt(px(32.))
                    .mb(px(8.))
                    .child(section_label("Up next", t.text_muted)),
            )
            .children(rows)
        });

    let lyrics = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .p(px(24.))
        .rounded(px(14.))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .child(app.lyrics_panel.clone());

    div()
        .flex()
        .flex_row()
        .gap(px(48.))
        .size_full()
        .py(px(40.))
        .px(px(48.))
        .bg(t.bg)
        .ui_font()
        .child(left)
        .child(lyrics)
}
