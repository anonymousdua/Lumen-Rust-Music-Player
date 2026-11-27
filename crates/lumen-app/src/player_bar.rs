//! The player bars, one view in three modes:
//!
//! - **Mini**: the 72px mini player under every library screen.
//! - **Transport**: the ~80px transport bar on Now Playing (shuffle, repeat,
//!   a 52px play button, a seek bar with a hover time, 110px volume).
//! - **FullScreen**: the borderless full-screen lyrics view (F11): cover,
//!   title, A− / A+, Exit, the lyrics, and play/pause with a seek bar.
//!
//! They are separate views from the library screens so the 4 Hz position
//! redraws stay here.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, Entity, FontWeight, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Point, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, WeakEntity, Window,
    canvas, div, px, relative,
};
use lumen_core::{LyricsSettings, RepeatMode, Track};

use crate::app::{LumenApp, Screen};
use crate::assets::Icon;
use crate::cover::{CoverCache, cover, placeholder};
use crate::lyrics_view::LyricsView;
use crate::player::Player;
use crate::songs::khz;
use crate::theme::Theme;
use crate::ui::{FontExt, format_time, icon};
use crate::widgets::{secondary_button, text_tooltip};

pub const MINI_HEIGHT: f32 = 72.0;
pub const TRANSPORT_HEIGHT: f32 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarMode {
    Mini,
    Transport,
    FullScreen,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// Seeking; the bar previews this fraction until release.
    Seek(f32),
    Volume,
}

pub struct PlayerBar {
    mode: BarMode,
    player: Entity<Player>,
    app: WeakEntity<LumenApp>,
    covers: Entity<CoverCache>,
    thumbs: PathBuf,
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Drag>,
    /// Mouse over the seek bar, as a fraction (for the time tooltip).
    hover: Option<f32>,
    /// The full-screen lyrics (FullScreen mode only).
    lyrics: Option<Entity<LyricsView>>,
}

impl PlayerBar {
    pub fn new(
        mode: BarMode,
        player: Entity<Player>,
        app: WeakEntity<LumenApp>,
        covers: Entity<CoverCache>,
        thumbs: PathBuf,
        lyrics: Option<Entity<LyricsView>>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&player, |_, _, cx| cx.notify()).detach();
        Self {
            mode,
            player,
            app,
            covers,
            thumbs,
            seek_bounds: Rc::default(),
            volume_bounds: Rc::default(),
            drag: None,
            hover: None,
            lyrics,
        }
    }

    fn fraction(bounds: Bounds<Pixels>, at: Point<Pixels>) -> f32 {
        let w = f32::from(bounds.size.width);
        if w <= 0.0 {
            return 0.0;
        }
        (f32::from(at.x - bounds.left()) / w).clamp(0.0, 1.0)
    }

    fn start_seek(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.player.read(cx).track().is_none() {
            return;
        }
        let f = Self::fraction(self.seek_bounds.get(), e.position);
        self.drag = Some(Drag::Seek(f));
        cx.notify();
    }

    fn hover_seek(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let f = Self::fraction(self.seek_bounds.get(), e.position);
        if self.hover != Some(f) {
            self.hover = Some(f);
            cx.notify();
        }
    }

    fn start_volume(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag = Some(Drag::Volume);
        let f = Self::fraction(self.volume_bounds.get(), e.position);
        self.player.update(cx, |p, cx| p.set_volume(f, cx));
    }

    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        match self.drag {
            Some(Drag::Seek(_)) => {
                let f = Self::fraction(self.seek_bounds.get(), at);
                self.drag = Some(Drag::Seek(f));
                self.hover = Some(f);
                cx.notify();
            }
            Some(Drag::Volume) => {
                let f = Self::fraction(self.volume_bounds.get(), at);
                self.player.update(cx, |p, cx| p.set_volume(f, cx));
            }
            None => {}
        }
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        match self.drag.take() {
            Some(Drag::Seek(f)) => self.player.update(cx, |p, cx| p.seek_fraction(f, cx)),
            Some(Drag::Volume) => self.player.update(cx, |p, _| p.save_resume()),
            None => {}
        }
        cx.notify();
    }

    /// While dragging, follow the mouse anywhere in the window.
    fn drag_follower(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let dragging = self.drag.is_some();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                if !dragging {
                    return;
                }
                let mv = this.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        mv.update(cx, |m, cx| m.drag_to(e.position, cx));
                    }
                });
                let up = this.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        up.update(cx, |m, cx| m.end_drag(cx));
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    /// Elapsed time, the seek bar (click or drag; hover shows the time in
    /// Transport and FullScreen modes) and the total.
    fn progress(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = self.player.read(cx);
        let has_track = p.track().is_some();
        let total = p.total();
        let shown = match self.drag {
            Some(Drag::Seek(f)) => total.mul_f32(f),
            _ => p.position.min(total.max(p.position)),
        };
        let fraction = if total.is_zero() {
            0.0
        } else {
            (shown.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
        };
        let hover_time = (self.mode != BarMode::Mini && has_track)
            .then_some(self.hover)
            .flatten()
            .map(|f| (f, format_time(total.mul_f32(f).as_secs())));
        let bar_w = self.seek_bounds.get().size.width;

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .flex_1()
            .min_w(px(120.))
            .tabular_nums()
            .text_size(px(12.))
            .text_color(t.text_3)
            .child(div().flex_none().child(format_time(shown.as_secs())))
            .child(
                hit_bar(fraction, self.seek_bounds.clone(), t)
                    .id("seek-bar")
                    .flex_1()
                    .when(has_track, |d| d.cursor_pointer())
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::start_seek))
                    .on_mouse_move(cx.listener(Self::hover_seek))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered && this.hover.is_some() && this.drag.is_none() {
                            this.hover = None;
                            cx.notify();
                        }
                    }))
                    .when_some(hover_time, |d, (f, label)| {
                        d.child(
                            div()
                                .absolute()
                                .bottom(px(20.))
                                .left(bar_w * f - px(24.))
                                .w(px(48.))
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .px(px(6.))
                                        .py(px(2.))
                                        .rounded(px(6.))
                                        .bg(t.surface_2)
                                        .border_1()
                                        .border_color(t.border_input)
                                        .text_size(px(11.))
                                        .text_color(t.text)
                                        .child(label),
                                ),
                        )
                    }),
            )
            .child(div().flex_none().child(format_time(total.as_secs())))
    }

    /// Volume icon and bar, with a tooltip that explains exclusive mode.
    fn volume(&self, width: f32, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = self.player.read(cx);
        let volume = p.volume;
        let mut tip = format!("Volume {}%", (volume * 100.0).round() as u32);
        if p.exclusive() {
            tip.push_str(
                "\nWASAPI exclusive: volume is applied digitally before output. \
                 Below 100% the output is no longer bit-perfect.",
            );
        }
        div()
            .id("volume")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .flex_none()
            .tooltip(move |_, cx| text_tooltip(tip.clone(), cx))
            .child(icon(
                if volume <= 0.0 {
                    Icon::VolumeMute
                } else {
                    Icon::Volume
                },
                px(18.),
                t.text_3,
            ))
            .child(
                hit_bar(volume, self.volume_bounds.clone(), t)
                    .id("volume-bar")
                    .w(px(width))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::start_volume)),
            )
    }

    fn play_button(
        &self,
        size: f32,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let playing = self.player.read(cx).is_playing();
        let hover = t.accent_hover;
        div()
            .id("play-pause")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(size))
            .rounded_full()
            .bg(t.accent)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(|this, _, _, cx| {
                this.player.update(cx, |p, _| p.toggle());
            }))
            .child(icon(
                if playing { Icon::Pause } else { Icon::Play },
                px(size * 0.4),
                t.on_accent,
            ))
    }

    fn prev_next(&self, next: bool, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let has_track = self.player.read(cx).track().is_some();
        let (id, glyph) = if next {
            ("next", Icon::SkipForward)
        } else {
            ("prev", Icon::SkipBack)
        };
        round_button(id, glyph, has_track, false, t).on_click(cx.listener(move |this, _, _, cx| {
            this.player
                .update(cx, |p, _| if next { p.next() } else { p.prev() });
        }))
    }

    fn render_mini(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let track = self.player.read(cx).track().cloned();
        let message = self.player.read(cx).message.clone();
        let (title, sub): (SharedString, SharedString) = match &track {
            Some(tr) => (
                tr.title.clone().into(),
                format!("{} · {}", tr.artist, format_short(tr)).into(),
            ),
            None => (
                "Nothing playing".into(),
                message
                    .unwrap_or_else(|| "Double-click a song to play it".into())
                    .into(),
            ),
        };
        let art = self.art(track.as_ref(), 44., 6., t);
        let app = self.app.clone();
        let app2 = self.app.clone();

        div()
            .id("mini-player")
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap(px(24.))
            .h(px(MINI_HEIGHT))
            .px(px(16.))
            .bg(t.surface)
            .border_t_1()
            .border_color(t.border)
            .child(self.drag_follower(cx))
            // Track info: opens Now Playing.
            .child(
                div()
                    .id("mini-now-playing")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .w(px(260.))
                    .flex_none()
                    .min_w_0()
                    .p(px(4.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover({
                        let bg = t.surface_hover;
                        move |s| s.bg(bg)
                    })
                    .on_click(move |_, _, cx| {
                        let _ = app.update(cx, |a, cx| a.navigate(Screen::NowPlaying, cx));
                    })
                    .child(art)
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
                                    .text_color(if track.is_some() { t.text } else { t.text_2 })
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(t.text_muted)
                                    .truncate()
                                    .child(sub),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .flex_none()
                    .child(self.prev_next(false, t, cx))
                    .child(self.play_button(44., t, cx))
                    .child(self.prev_next(true, t, cx)),
            )
            .child(self.progress(t, cx))
            .child(
                div()
                    .id("mini-lyrics")
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(32.))
                    .px(px(14.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.border_input)
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.text_2)
                    .cursor_pointer()
                    .hover({
                        let bg = t.button_hover;
                        move |s| s.bg(bg)
                    })
                    .on_click(move |_, window, cx| {
                        let _ = app2.update(cx, |a, cx| a.set_fullscreen(true, window, cx));
                    })
                    .child("Lyrics"),
            )
            .child(self.volume(90., t, cx))
            .into_any_element()
    }

    fn render_transport(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = self.player.read(cx);
        let (shuffle, repeat) = (p.shuffle, p.repeat);
        // Radio: shown while recommendations are on.
        let radio = self
            .app
            .upgrade()
            .map(|a| a.read(cx))
            .filter(|a| a.reco_on())
            .map(|a| a.settings.reco.radio);
        div()
            .id("transport")
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap(px(28.))
            .h(px(TRANSPORT_HEIGHT))
            .px(px(24.))
            .bg(t.surface)
            .border_t_1()
            .border_color(t.border)
            .child(self.drag_follower(cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .flex_none()
                    .child(
                        round_button("shuffle", Icon::Shuffle, true, shuffle, t)
                            .tooltip(|_, cx| text_tooltip("Shuffle", cx))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.player.update(cx, |p, cx| p.toggle_shuffle(cx));
                            })),
                    )
                    .child(self.prev_next(false, t, cx))
                    .child(self.play_button(52., t, cx))
                    .child(self.prev_next(true, t, cx))
                    .child(
                        round_button(
                            "repeat",
                            if repeat == RepeatMode::One {
                                Icon::RepeatOne
                            } else {
                                Icon::Repeat
                            },
                            true,
                            repeat != RepeatMode::Off,
                            t,
                        )
                        .tooltip(|_, cx| text_tooltip("Repeat: off, all, one", cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.player.update(cx, |p, cx| p.cycle_repeat(cx));
                        })),
                    )
                    .when_some(radio, |d, on| {
                        d.child(
                            round_button("radio", Icon::Radio, true, on, t)
                                .tooltip(|_, cx| {
                                    text_tooltip("Radio: keep playing similar songs", cx)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(app) = this.app.upgrade() {
                                        app.update(cx, |a, cx| a.toggle_radio(cx));
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(self.progress(t, cx))
            .child(self.volume(110., t, cx))
            .into_any_element()
    }

    fn render_fullscreen(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let track = self.player.read(cx).track().cloned();
        let (title, sub): (SharedString, SharedString) = match &track {
            Some(tr) => (
                tr.title.clone().into(),
                format!("{} · {}", tr.artist, tr.album).into(),
            ),
            None => ("Nothing playing".into(), SharedString::default()),
        };
        let art = self.art(track.as_ref(), 48., 8., t);
        let app = self.app.clone();
        let (app_a, app_b) = (self.app.clone(), self.app.clone());
        let scale = self.player.read(cx).lyrics_prefs.fullscreen_scale;
        let lyrics: AnyElement = match &self.lyrics {
            Some(l) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(l.clone())
                .into_any_element(),
            None => div().flex_1().into_any_element(),
        };
        div()
            .id("fullscreen")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .px(px(48.))
            .py(px(32.))
            .bg(t.bg)
            .child(self.drag_follower(cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(14.))
                    .child(art)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .display_font()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(t.text_muted)
                                    .truncate()
                                    .child(sub),
                            ),
                    )
                    .child(
                        secondary_button(
                            "text-smaller",
                            None,
                            "A−",
                            scale > LyricsSettings::SCALE_MIN + 0.01,
                            t,
                        )
                        .tooltip(|_, cx| text_tooltip("Smaller lyrics", cx))
                        .on_click(move |_, _, cx| {
                            let _ = app_a.update(cx, |a, cx| a.step_lyrics_scale(false, cx));
                        }),
                    )
                    .child(
                        secondary_button(
                            "text-larger",
                            None,
                            "A+",
                            scale < LyricsSettings::SCALE_MAX - 0.01,
                            t,
                        )
                        .tooltip(|_, cx| text_tooltip("Larger lyrics", cx))
                        .on_click(move |_, _, cx| {
                            let _ = app_b.update(cx, |a, cx| a.step_lyrics_scale(true, cx));
                        }),
                    )
                    .child(
                        secondary_button("exit-fullscreen", None, "Exit full screen", true, t)
                            .on_click(move |_, window, cx| {
                                let _ = app.update(cx, |a, cx| a.set_fullscreen(false, window, cx));
                            }),
                    ),
            )
            .child(lyrics)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap(px(24.))
                    .child(self.play_button(52., t, cx))
                    .child(div().w(px(560.)).flex().child(self.progress(t, cx))),
            )
            .into_any_element()
    }

    fn art(&self, track: Option<&Track>, size: f32, radius: f32, t: &Theme) -> AnyElement {
        match track {
            Some(tr) => cover(
                tr.cover_hash.as_deref(),
                &self.thumbs,
                &tr.album,
                px(size),
                px(radius),
                t,
            ),
            None => placeholder("", px(size), px(radius), t).into_any_element(),
        }
    }
}

/// "FLAC 24/96", "FLAC 16/44.1", "MP3".
fn format_short(t: &Track) -> String {
    match (t.is_lossless(), t.bit_depth, t.sample_rate) {
        (true, Some(b), Some(r)) => format!("{} {b}/{}", t.codec, khz(r)),
        _ => t.codec.clone(),
    }
}

impl Render for PlayerBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::global(cx).clone();
        let body = match self.mode {
            BarMode::Mini => self.render_mini(&t, cx),
            BarMode::Transport => self.render_transport(&t, cx),
            BarMode::FullScreen => self.render_fullscreen(&t, cx),
        };
        let full = self.mode == BarMode::FullScreen;
        div()
            .image_cache(self.covers.clone())
            .ui_font()
            .text_color(t.text)
            .when(full, |d| d.size_full())
            .child(body)
    }
}

/// 44×44 round icon button; `active` toggles are accent.
fn round_button(
    id: &'static str,
    glyph: Icon,
    enabled: bool,
    active: bool,
    t: &Theme,
) -> Stateful<gpui::Div> {
    let hover_bg = t.button_hover;
    let color = if !enabled {
        t.text_muted
    } else if active {
        t.accent
    } else if matches!(
        glyph,
        Icon::Shuffle | Icon::Repeat | Icon::RepeatOne | Icon::Radio
    ) {
        t.text_3
    } else {
        t.text_2
    };
    let d = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(44.))
        .rounded_full()
        .child(icon(glyph, px(18.), color));
    if enabled {
        d.cursor_pointer().hover(move |s| s.bg(hover_bg))
    } else {
        d
    }
}

/// A 4px bar (`track` background, `accent` fill) inside a 16px-tall hit
/// area. Records its bounds so clicks map to a fraction.
fn hit_bar(fraction: f32, bounds: Rc<Cell<Bounds<Pixels>>>, t: &Theme) -> gpui::Div {
    div()
        .relative()
        .flex()
        .items_center()
        .h(px(16.))
        .child(
            canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(
            div().w_full().h(px(4.)).rounded_full().bg(t.track).child(
                div()
                    .h_full()
                    .rounded_full()
                    .bg(t.accent)
                    .w(relative(fraction.clamp(0.0, 1.0))),
            ),
        )
}
