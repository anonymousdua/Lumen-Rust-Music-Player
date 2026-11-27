//! The lyrics view, in two modes:
//!
//! - **Panel** (Now Playing): header with "Full screen" and the ±0.5 s offset
//!   buttons; every line in a scrolling list that keeps the active line
//!   about 40% down, pausing for 4 s after you scroll with the wheel.
//! - **FullScreen**: seven lines centered around the active one, scaled by
//!   A− / A+.
//!
//! The active line grows and turns accent over 350 ms; the line it replaces
//! shrinks back. With "Word-by-word highlight" on and word timings present,
//! the sung part of the active line fills with accent. Clicking a line
//! seeks to it.
//!
//! It listens to the player's 30 Hz ticks itself and redraws only when the
//! active line changes, a word fill is in progress, or a scroll is settling.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Entity, FontWeight, HighlightStyle, Hsla,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, StyledText, WeakEntity, Window, div, ease_in_out, point,
    px,
};
use lumen_lyrics::Lyrics;
use unicode_segmentation::UnicodeSegmentation;

use crate::app::LumenApp;
use crate::player::{Player, PlayerEvent};
use crate::popup::format_offset;
use crate::theme::Theme;
use crate::ui::{FontExt, section_label};
use crate::widgets::{secondary_button, text_tooltip};

const TRANSITION: Duration = Duration::from_millis(350);
/// Wheel scrolling pauses auto-scroll for this long.
const MANUAL_SCROLL_HOLD: Duration = Duration::from_secs(4);
/// The active line sits this far down the panel.
const ANCHOR: f32 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LyricsMode {
    Panel,
    FullScreen,
}

pub struct LyricsView {
    mode: LyricsMode,
    player: Entity<Player>,
    app: WeakEntity<LumenApp>,
    scroll: ScrollHandle,
    /// Lyrics the state below belongs to (pointer identity).
    lyrics: Option<Arc<Lyrics>>,
    active: Option<usize>,
    prev_active: Option<usize>,
    /// Bumped on every active-line change; restarts the line animations.
    epoch: usize,
    manual_until: Option<Instant>,
    settling: bool,
    /// Panel height the spacers were sized with (layout catches up a frame
    /// later after the panel appears or resizes).
    panel_h: Pixels,
    /// Last drawn word fill: (line, sung bytes).
    last_fill: Option<(usize, usize)>,
    /// Scroll animation in progress: start offset and start time.
    scroll_from: Option<(Pixels, Instant)>,
}

/// Sizes and colors for one mode.
struct Look {
    active_size: f32,
    other_size: f32,
    past: Hsla,
    future: Hsla,
    gap: f32,
}

impl LyricsView {
    pub fn new(
        mode: LyricsMode,
        player: Entity<Player>,
        app: WeakEntity<LumenApp>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.subscribe(&player, |this, _, e: &PlayerEvent, cx| match e {
            PlayerEvent::Tick => this.on_tick(cx),
            PlayerEvent::Changed => cx.notify(),
            PlayerEvent::Error => {}
        })
        .detach();
        Self {
            mode,
            player,
            app,
            scroll: ScrollHandle::new(),
            lyrics: None,
            active: None,
            prev_active: None,
            epoch: 0,
            manual_until: None,
            settling: false,
            panel_h: px(0.),
            last_fill: None,
            scroll_from: None,
        }
    }

    /// Recomputes the active line; true if it changed.
    fn update_active(&mut self, cx: &mut Context<Self>) -> bool {
        let p = self.player.read(cx);
        let lyrics = p.lyrics.clone();
        let t = p.lyrics_time_ms();
        let same = match (&lyrics, &self.lyrics) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.lyrics = lyrics.clone();
            self.active = None;
            self.prev_active = None;
            self.manual_until = None;
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        let now = lyrics.as_ref().and_then(|l| l.line_at(t));
        if now != self.active {
            self.prev_active = self.active;
            self.active = now;
            self.epoch += 1;
            self.settling = true;
            self.scroll_from = None;
            return true;
        }
        !same
    }

    fn on_tick(&mut self, cx: &mut Context<Self>) {
        let changed = self.update_active(cx);
        // Word fill: redraw only when the filled part grows by a grapheme,
        // not on every 30 Hz tick.
        let mut fill_moved = false;
        if self.word_mode(cx)
            && let (Some(a), Some(l)) = (self.active, self.lyrics.clone())
        {
            let (k, frac) = l.word_progress(a, self.player.read(cx).lyrics_time_ms());
            let line = &l.lines[a];
            let sung = sung_bytes(&line.text, &line.words, k, frac);
            fill_moved = self.last_fill != Some((a, sung));
            self.last_fill = Some((a, sung));
        }
        if changed || fill_moved || self.settling {
            cx.notify();
        }
    }

    fn word_mode(&self, cx: &Context<Self>) -> bool {
        let p = self.player.read(cx);
        p.lyrics_prefs.word_highlight && self.lyrics.as_ref().is_some_and(|l| l.has_words())
    }

    fn look(&self, cx: &Context<Self>, t: &Theme) -> Look {
        match self.mode {
            LyricsMode::Panel => Look {
                active_size: 34.,
                other_size: 24.,
                past: t.lyric_past,
                future: t.lyric_future,
                gap: 10.,
            },
            LyricsMode::FullScreen => {
                let s = self.player.read(cx).lyrics_prefs.fullscreen_scale;
                Look {
                    active_size: 56. * s,
                    other_size: 32. * s,
                    past: t.lyric_past_fullscreen,
                    future: t.lyric_future_fullscreen,
                    gap: 18. * s,
                }
            }
        }
    }

    /// One synced line: size and color by state, animated on change; the
    /// active line gets the word fill.
    fn line(&self, i: usize, look: &Look, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(lyrics) = self.lyrics.clone() else {
            return div().into_any_element();
        };
        let line = &lyrics.lines[i];
        let p = self.player.read(cx);
        let font = p.lyrics_prefs.font;
        let is_active = self.active == Some(i);
        let was_active = self.prev_active == Some(i);
        let past = self.active.is_some_and(|a| i < a);
        let rest_color = if past { look.past } else { look.future };
        let words = is_active && p.lyrics_prefs.word_highlight && line.words.len() > 1;
        let text: SharedString = if line.text.is_empty() {
            "♪".into()
        } else {
            line.text.clone().into()
        };

        let content: AnyElement = if words {
            let (k, frac) = lyrics.word_progress(i, p.lyrics_time_ms());
            let sung = sung_bytes(&line.text, &line.words, k, frac);
            StyledText::new(text.clone())
                .with_highlights([(
                    0..sung,
                    HighlightStyle {
                        color: Some(t.accent),
                        ..Default::default()
                    },
                )])
                .into_any_element()
        } else {
            text.clone().into_any_element()
        };
        let time = line.time_ms;
        let player = self.player.clone();
        let base = div()
            .id(("lyric", i))
            .flex_none()
            .lyrics_font(font)
            .cursor_pointer()
            .when(self.mode == LyricsMode::FullScreen, |d| d.text_center())
            .when_some(time, |d, ms| {
                d.on_click(move |_, _, cx| {
                    player.update(cx, |p, cx| p.seek_to_lyrics(ms, cx));
                })
            })
            .font_weight(if is_active {
                FontWeight::BOLD
            } else {
                FontWeight::SEMIBOLD
            })
            .child(content);

        let (big, small) = (look.active_size, look.other_size);
        // While word-filling, the unsung part stays light; the fill is accent.
        let active_color = if words { t.text_2 } else { t.accent };
        if is_active {
            let from = look.future;
            base.with_animation(
                ("lyric-in", self.epoch * 10_000 + i),
                Animation::new(TRANSITION).with_easing(ease_in_out),
                move |d, k| {
                    let size = small + (big - small) * k;
                    d.text_size(px(size))
                        .line_height(px(size * 1.3))
                        .text_color(mix(from, active_color, k))
                },
            )
            .into_any_element()
        } else if was_active {
            base.with_animation(
                ("lyric-out", self.epoch * 10_000 + i),
                Animation::new(TRANSITION).with_easing(ease_in_out),
                move |d, k| {
                    let size = big + (small - big) * k;
                    d.text_size(px(size))
                        .line_height(px(size * 1.3))
                        .text_color(mix(active_color, rest_color, k))
                },
            )
            .into_any_element()
        } else {
            base.text_size(px(small))
                .line_height(px(small * 1.3))
                .text_color(rest_color)
                .into_any_element()
        }
    }

    /// Keeps the active line about 40% down the panel: a 350 ms eased
    /// scroll, started with each line change (in step with the line's own
    /// transition), then nothing until the next change.
    fn auto_scroll(&mut self, window: &mut Window) {
        let now = Instant::now();
        if self.manual_until.is_some_and(|u| now < u) {
            self.settling = false;
            self.scroll_from = None;
            return;
        }
        let Some(a) = self.active else {
            self.settling = false;
            return;
        };
        // Child 0 is the top spacer.
        let (Some(item), view) = (self.scroll.bounds_for_item(a + 1), self.scroll.bounds()) else {
            return;
        };
        let content_top = item.top() - view.top();
        let target = view.size.height * ANCHOR - content_top - item.size.height / 2.;
        let target = target.clamp(-self.scroll.max_offset().height, px(0.));
        if !self.settling {
            // Settled: follow layout changes (resize) without animating.
            if (self.scroll.offset().y - target).abs() > px(0.5) {
                self.scroll.set_offset(point(px(0.), target));
            }
            return;
        }
        let (from, start) = *self
            .scroll_from
            .get_or_insert((self.scroll.offset().y, now));
        let k = (now.duration_since(start).as_secs_f32() / TRANSITION.as_secs_f32()).min(1.0);
        let y = from + (target - from) * ease_in_out(k);
        self.scroll.set_offset(point(px(0.), y));
        if k >= 1.0 {
            self.settling = false;
            self.scroll_from = None;
        } else {
            window.request_animation_frame();
        }
    }

    fn header(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = self.player.read(cx);
        let offset = p.lyrics_offset_ms;
        let has_track = p.track().is_some();
        let link = t.accent_hover;
        let app = self.app.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.))
            .flex_none()
            .child(section_label("Lyrics · click a line to seek", t.text_muted))
            .child(
                div()
                    .id("lyrics-fullscreen")
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.accent)
                    .cursor_pointer()
                    .hover(move |s| s.text_color(link))
                    .on_click(move |_, window, cx| {
                        let _ = app.update(cx, |a, cx| a.set_fullscreen(true, window, cx));
                    })
                    .child("Full screen"),
            )
            .child(div().flex_1())
            .when(offset != 0, |d| {
                d.child(
                    div()
                        .tabular_nums()
                        .text_size(px(12.))
                        .text_color(t.text_muted)
                        .child(format_offset(offset)),
                )
            })
            .child(
                small_button("offset-minus", "Offset −0.5s", has_track, t)
                    .tooltip(|_, cx| text_tooltip("Show lyrics 0.5 s earlier", cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.player
                            .update(cx, |p, cx| p.nudge_lyrics_offset(-500, cx));
                    })),
            )
            .child(
                small_button("offset-plus", "+0.5s", has_track, t)
                    .tooltip(|_, cx| text_tooltip("Show lyrics 0.5 s later", cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.player
                            .update(cx, |p, cx| p.nudge_lyrics_offset(500, cx));
                    })),
            )
    }

    /// Muted message in place of lyrics.
    fn message(&self, text: &'static str, t: &Theme, look: &Look) -> AnyElement {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .px(px(24.))
            .text_center()
            .display_font()
            .text_size(px(if self.mode == LyricsMode::Panel {
                18.
            } else {
                look.other_size * 0.75
            }))
            .line_height(px(28.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.text_muted)
            .child(
                div()
                    .w_full()
                    .max_w(px(520.))
                    .whitespace_normal()
                    .text_center()
                    .child(text),
            )
            .into_any_element()
    }

    fn body(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let look = self.look(cx, t);
        let (has_track, loading, font) = {
            let p = self.player.read(cx);
            (p.track().is_some(), p.lyrics_loading(), p.lyrics_prefs.font)
        };
        let Some(lyrics) = self.lyrics.clone() else {
            return if !has_track {
                self.message("Nothing playing.", t, &look)
            } else if loading {
                div().flex_1().into_any_element()
            } else {
                self.message(
                    "No lyrics found. Put an .lrc file with the same name next to the audio file.",
                    t,
                    &look,
                )
            };
        };

        if !lyrics.synced {
            // Plain lyrics: static text that scrolls.
            let size = if self.mode == LyricsMode::Panel {
                22.
            } else {
                look.other_size
            };
            return div()
                .id("lyrics-plain")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .py(px(16.))
                .flex()
                .flex_col()
                .gap(px(look.gap / 2.))
                .when(self.mode == LyricsMode::FullScreen, |d| d.items_center())
                .lyrics_font(font)
                .text_size(px(size))
                .line_height(px(size * 1.4))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(look.future)
                .children(
                    lyrics
                        .lines
                        .iter()
                        .map(|l| div().child(SharedString::from(l.text.clone())))
                        .collect::<Vec<_>>(),
                )
                .into_any_element();
        }

        match self.mode {
            LyricsMode::Panel => {
                self.auto_scroll(window);
                let h = self.scroll.bounds().size.height;
                if (h - self.panel_h).abs() > px(0.5) {
                    self.panel_h = h;
                    self.settling = true;
                    window.request_animation_frame();
                }
                let mut children: Vec<AnyElement> = Vec::with_capacity(lyrics.lines.len() + 2);
                // Spacers let the first and last lines reach the anchor;
                // flex_none keeps the list from squeezing them away.
                children.push(div().flex_none().h(h * ANCHOR).into_any_element());
                for i in 0..lyrics.lines.len() {
                    children.push(self.line(i, &look, t, cx));
                }
                children.push(div().flex_none().h(h * (1.0 - ANCHOR)).into_any_element());
                div()
                    .id("lyrics-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .gap(px(look.gap))
                    .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                        this.manual_until = Some(Instant::now() + MANUAL_SCROLL_HOLD);
                        // Resume (and redraw) when the hold ends, even if paused.
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(MANUAL_SCROLL_HOLD + Duration::from_millis(50))
                                .await;
                            let _ = this.update(cx, |v, cx| {
                                v.settling = true;
                                v.scroll_from = None;
                                cx.notify();
                            });
                        })
                        .detach();
                    }))
                    .children(children)
                    .into_any_element()
            }
            LyricsMode::FullScreen => {
                // Seven lines: the active one in the middle.
                let n = lyrics.lines.len();
                let center = self.active.unwrap_or(0);
                let first = center.saturating_sub(3);
                let last = (first + 7).min(n);
                let first = last.saturating_sub(7);
                let mut lines = Vec::new();
                // Keep the active line centered near the start and the end.
                let pad_before = 3usize.saturating_sub(center - first);
                for _ in 0..pad_before.min(3) {
                    lines.push(div().h(px(look.other_size * 1.3)).into_any_element());
                }
                for i in first..last {
                    lines.push(self.line(i, &look, t, cx));
                }
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(look.gap))
                    .px(px(48.))
                    .children(lines)
                    .into_any_element()
            }
        }
    }
}

/// Byte length of the sung part of a line: `k` whole words plus `frac` of
/// the next one, cut at grapheme boundaries so Indic clusters stay whole.
fn sung_bytes(text: &str, words: &[lumen_lyrics::Word], k: usize, frac: f32) -> usize {
    let Some(first) = words.first() else {
        return 0;
    };
    // Untimed text before the first word counts as sung with it.
    let prefix = text.find(first.text.trim()).unwrap_or(0);
    let mut n = prefix
        + words[..k.min(words.len())]
            .iter()
            .map(|w| w.text.len())
            .sum::<usize>();
    if let Some(w) = words.get(k) {
        let g: Vec<&str> = w.text.graphemes(true).collect();
        let take = ((g.len() as f32) * frac.clamp(0.0, 1.0)).floor() as usize;
        n += g[..take.min(g.len())]
            .iter()
            .map(|s| s.len())
            .sum::<usize>();
    }
    let mut n = n.min(text.len());
    while !text.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// Linear color blend.
fn mix(a: Hsla, b: Hsla, k: f32) -> Hsla {
    let a = gpui::Rgba::from(a);
    let b = gpui::Rgba::from(b);
    let l = |x: f32, y: f32| x + (y - x) * k;
    gpui::Rgba {
        r: l(a.r, b.r),
        g: l(a.g, b.g),
        b: l(a.b, b.b),
        a: l(a.a, b.a),
    }
    .into()
}

/// 32px outline button for the offset controls.
fn small_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    t: &Theme,
) -> gpui::Stateful<gpui::Div> {
    secondary_button(id, None, label, enabled, t)
        .h(px(32.))
        .px(px(10.))
}

impl Render for LyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::global(cx).clone();
        self.update_active(cx);
        let body = self.body(&t, window, cx);
        match self.mode {
            LyricsMode::Panel => div()
                .flex()
                .flex_col()
                .gap(px(16.))
                .size_full()
                .child(self.header(&t, cx))
                .child(body),
            LyricsMode::FullScreen => div().flex().flex_col().size_full().child(body),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sung_bytes;
    use lumen_lyrics::parse_lrc;

    #[test]
    fn sung_part_respects_graphemes() {
        let l = parse_lrc("[00:01.00]<00:01.00>നിലാ <00:02.00>വിന്റെ<00:03.00>");
        let line = &l.lines[0];
        assert_eq!(sung_bytes(&line.text, &line.words, 0, 0.0), 0);
        assert_eq!(
            &line.text[..sung_bytes(&line.text, &line.words, 1, 0.0)],
            "നിലാ "
        );
        let half = sung_bytes(&line.text, &line.words, 1, 0.5);
        assert!(line.text.is_char_boundary(half) && half > "നിലാ ".len());
        assert_eq!(sung_bytes(&line.text, &line.words, 2, 0.0), line.text.len());
    }
}
