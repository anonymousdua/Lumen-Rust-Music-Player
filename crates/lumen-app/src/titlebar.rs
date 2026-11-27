//! Custom title bar for the frameless window.
//!
//! Dragging, double-click-to-maximize, Aero Snap and the Windows 11 snap-layout
//! flyout are all handled natively: GPUI answers `WM_NCHITTEST` with `HTCAPTION`,
//! `HTMINBUTTON`, `HTMAXBUTTON` and `HTCLOSE` for the areas marked with
//! `window_control_area`, so Windows treats them exactly like a native caption.

use gpui::{
    FontWeight, InteractiveElement, IntoElement, ParentElement, Styled, Window, WindowControlArea,
    div, px,
};

use crate::assets::Icon;
use crate::theme::Theme;
use crate::ui::{FontExt, icon};

pub const HEIGHT: f32 = 40.0;
const CAPTION_BUTTON_WIDTH: f32 = 46.0;

pub fn render(window: &Window, t: &Theme) -> impl IntoElement {
    let maximize_icon = if window.is_maximized() {
        Icon::CaptionRestore
    } else {
        Icon::CaptionMaximize
    };

    div()
        .id("titlebar")
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .w_full()
        .h(px(HEIGHT))
        .bg(t.bg_titlebar)
        .border_b_1()
        .border_color(t.border)
        .child(
            // The drag region is a sibling of the caption buttons, not their parent:
            // GPUI reports the first control area under the cursor, so nesting the
            // buttons inside a Drag area would make them act as caption.
            div()
                .id("titlebar-drag")
                .flex()
                .flex_row()
                .items_center()
                .flex_1()
                .h_full()
                .gap(px(10.))
                .pl(px(16.))
                .window_control_area(WindowControlArea::Drag)
                .child(
                    // Amber logo mark.
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(20.))
                        .rounded(px(6.))
                        .bg(t.accent)
                        .child(icon(Icon::Logo, px(13.), t.on_accent)),
                )
                .child(
                    div()
                        .display_font()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .child("Lumen"),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .h_full()
                .child(caption_button(
                    "caption-min",
                    Icon::CaptionMinimize,
                    WindowControlArea::Min,
                    t,
                    false,
                ))
                .child(caption_button(
                    "caption-max",
                    maximize_icon,
                    WindowControlArea::Max,
                    t,
                    false,
                ))
                .child(caption_button(
                    "caption-close",
                    Icon::CaptionClose,
                    WindowControlArea::Close,
                    t,
                    true,
                )),
        )
}

fn caption_button(
    id: &'static str,
    glyph: Icon,
    area: WindowControlArea,
    t: &Theme,
    is_close: bool,
) -> impl IntoElement {
    let (hover_bg, hover_glyph) = if is_close {
        (t.caption_close_hover, t.caption_close_glyph)
    } else {
        (t.button_hover, t.text)
    };
    div()
        .id(id)
        .group(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(CAPTION_BUTTON_WIDTH))
        .h_full()
        .window_control_area(area)
        .hover(move |s| s.bg(hover_bg))
        .child(icon(glyph, px(10.), t.text_3).group_hover(id, move |s| s.text_color(hover_glyph)))
}
