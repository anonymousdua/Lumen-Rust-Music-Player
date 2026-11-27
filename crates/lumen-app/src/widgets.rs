//! Controls shared by the library screens: titles, buttons, selects, chips
//! and popup menus. Sizes and colors follow the design spec.

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, Stateful, Styled, div, px,
};

use crate::assets::Icon;
use crate::theme::Theme;
use crate::ui::{FontExt, icon};

/// "Songs" (Space Grotesk 32/700).
pub fn page_title(text: impl Into<SharedString>, t: &Theme) -> Div {
    div()
        .display_font()
        .text_size(px(32.))
        .line_height(px(40.))
        .font_weight(FontWeight::BOLD)
        .text_color(t.text)
        .truncate()
        .child(text.into())
}

/// The muted line under a page title.
pub fn subline(text: impl Into<SharedString>, t: &Theme) -> Div {
    div()
        .tabular_nums()
        .text_size(px(13.))
        .line_height(px(19.))
        .text_color(t.text_muted)
        .truncate()
        .child(text.into())
}

/// 38px select: "Sort by  Date modified ⌄".
pub fn select(
    id: impl Into<ElementId>,
    prefix: Option<&'static str>,
    value: impl Into<SharedString>,
    open: bool,
    t: &Theme,
) -> Stateful<Div> {
    let hover = t.button_hover;
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(38.))
        .pl(px(12.))
        .pr(px(10.))
        .rounded(px(8.))
        .bg(if open { t.button_hover } else { t.surface_2 })
        .border_1()
        .border_color(t.border_input)
        .text_size(px(13.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .when_some(prefix, |d, p| {
            d.child(div().text_color(t.text_muted).child(p))
        })
        .child(
            div()
                .text_color(t.text)
                .font_weight(FontWeight::MEDIUM)
                .child(value.into()),
        )
        .child(icon(Icon::ChevronDown, px(14.), t.text_3))
}

/// 38px secondary (outline) button with an optional icon.
pub fn secondary_button(
    id: impl Into<ElementId>,
    glyph: Option<Icon>,
    label: impl Into<SharedString>,
    enabled: bool,
    t: &Theme,
) -> Stateful<Div> {
    let hover = t.button_hover;
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(38.))
        .px(px(14.))
        .rounded(px(8.))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border_input)
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.text)
        .when(enabled, |b| b.cursor_pointer().hover(move |s| s.bg(hover)))
        .when(!enabled, |b| b.opacity(0.45))
        .when_some(glyph, |d, g| d.child(icon(g, px(16.), t.text_2)))
        .child(label.into())
}

/// 38px accent button ("Play all").
pub fn accent_button(
    id: impl Into<ElementId>,
    glyph: Option<Icon>,
    label: impl Into<SharedString>,
    enabled: bool,
    t: &Theme,
) -> Stateful<Div> {
    let hover = t.accent_hover;
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(38.))
        .px(px(16.))
        .rounded(px(8.))
        .bg(t.accent)
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.on_accent)
        .when(enabled, |b| b.cursor_pointer().hover(move |s| s.bg(hover)))
        .when(!enabled, |b| b.opacity(0.45))
        .when_some(glyph, |d, g| d.child(icon(g, px(14.), t.on_accent)))
        .child(label.into())
}

/// 32px filter chip.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    t: &Theme,
) -> Stateful<Div> {
    let hover = t.surface_hover;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(32.))
        .px(px(14.))
        .rounded_full()
        .border_1()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .map(|d| {
            if active {
                d.bg(t.accent_bg_2)
                    .border_color(t.accent_border)
                    .text_color(t.accent)
            } else {
                d.border_color(t.border_soft)
                    .text_color(t.text_2)
                    .hover(move |s| s.bg(hover))
            }
        })
        .child(label.into())
}

/// Back arrow with the name of where it goes ("← Albums").
pub fn back_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    t: &Theme,
) -> Stateful<Div> {
    let hover = t.button_hover;
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(32.))
        .pl(px(6.))
        .pr(px(12.))
        .rounded(px(8.))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.text_2)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(icon(Icon::Back, px(16.), t.text_2))
        .child(label.into())
}

/// A popup menu panel.
pub fn menu_panel(t: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w(px(200.))
        .p(px(4.))
        .rounded(px(10.))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border_input)
        .shadow_lg()
        .ui_font()
        .text_size(px(13.))
}

/// One row of a popup menu. `checked` shows a check mark (select menus).
pub fn menu_item(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    checked: Option<bool>,
    enabled: bool,
    t: &Theme,
) -> Stateful<Div> {
    let hover = t.button_hover;
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .h(px(32.))
        .px(px(10.))
        .rounded(px(6.))
        .text_color(if enabled { t.text } else { t.text_muted })
        .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(hover)))
        .when_some(checked, |d, on| {
            d.child(
                div()
                    .flex_none()
                    .w(px(16.))
                    .when(on, |d| d.child(icon(Icon::Check, px(14.), t.accent))),
            )
        })
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
}

pub fn menu_separator(t: &Theme) -> AnyElement {
    div()
        .h(px(1.))
        .my(px(4.))
        .mx(px(6.))
        .bg(t.border_input)
        .into_any_element()
}

/// A small text tooltip; a newline starts a new line.
pub struct TextTooltip {
    text: SharedString,
}

impl gpui::Render for TextTooltip {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = Theme::global(cx);
        div()
            .max_w(px(280.))
            .px(px(10.))
            .py(px(6.))
            .rounded(px(8.))
            .bg(t.surface_2)
            .border_1()
            .border_color(t.border_input)
            .shadow_md()
            .ui_font()
            .text_size(px(12.))
            .line_height(px(17.))
            .text_color(t.text_2)
            .children(
                self.text
                    .lines()
                    .map(|l| div().child(SharedString::from(l.to_string())))
                    .collect::<Vec<_>>(),
            )
    }
}

/// Builds a tooltip view for `.tooltip(..)`.
pub fn text_tooltip(text: impl Into<SharedString>, cx: &mut gpui::App) -> gpui::AnyView {
    let text = text.into();
    gpui::AppContext::new(cx, |_| TextTooltip { text }).into()
}
