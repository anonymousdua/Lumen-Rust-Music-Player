//! Cover art: thumbnails from the library's cache, or the colored placeholder.
//!
//! [`CoverCache`] keeps the most recently drawn thumbnails decoded (an LRU),
//! so scrolling a large album grid doesn't grow memory without bound.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures::FutureExt;
use gpui::{
    AnyElement, App, Asset, AssetLogger, Context, ImageAssetLoader, ImageCache, ImageCacheError,
    ImageCacheItem, IntoElement, ObjectFit, ParentElement, Pixels, RenderImage, Resource, Styled,
    StyledImage, Window, div, hash, img,
};

use crate::assets::Icon;
use crate::theme::Theme;
use crate::ui::icon;

/// Decoded thumbnails kept in memory. A 320 px thumbnail is about 400 KB.
const CAPACITY: usize = 240;

pub struct CoverCache {
    items: HashMap<u64, (ImageCacheItem, u64)>,
    tick: u64,
}

impl CoverCache {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            items: HashMap::new(),
            tick: 0,
        }
    }

    fn evict(&mut self, window: &mut Window, cx: &mut App) {
        while self.items.len() >= CAPACITY {
            let Some((&oldest, _)) = self.items.iter().min_by_key(|(_, (_, used))| *used) else {
                return;
            };
            if let Some((mut item, _)) = self.items.remove(&oldest)
                && let Some(Ok(image)) = item.get()
            {
                cx.drop_image(image, Some(window));
            }
        }
    }
}

impl ImageCache for CoverCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.tick += 1;
        let key = hash(resource);
        if let Some((item, used)) = self.items.get_mut(&key) {
            *used = self.tick;
            return item.get();
        }
        self.evict(window, cx);

        let fut = AssetLogger::<ImageAssetLoader>::load(resource.clone(), cx);
        let task = cx.background_executor().spawn(fut).shared();
        self.items
            .insert(key, (ImageCacheItem::Loading(task.clone()), self.tick));
        let view = window.current_view();
        window
            .spawn(cx, async move |cx| {
                let _ = task.await;
                cx.on_next_frame(move |_, cx| cx.notify(view));
            })
            .detach();
        None
    }
}

/// A square cover: the thumbnail if there is one, else the placeholder
/// colored from the palette by `name`.
pub fn cover(
    hash: Option<&str>,
    thumbs: &Path,
    name: &str,
    size: Pixels,
    radius: Pixels,
    t: &Theme,
) -> AnyElement {
    let placeholder = placeholder(name, size, radius, t);
    match hash {
        Some(h) => {
            let path = thumbs.join(format!("{h}.jpg"));
            let fallback = placeholder.clone();
            img(path)
                .size(size)
                .flex_none()
                .rounded(radius)
                .object_fit(ObjectFit::Cover)
                .with_loading(move || fallback.clone().into_any_element())
                .with_fallback(move || placeholder.clone().into_any_element())
                .into_any_element()
        }
        None => placeholder.into_any_element(),
    }
}

/// Placeholder art: palette background, matching icon, 1px border.
#[derive(Clone, IntoElement)]
pub struct Placeholder {
    bg: gpui::Hsla,
    fg: gpui::Hsla,
    border: gpui::Hsla,
    size: Pixels,
    radius: Pixels,
}

pub fn placeholder(name: &str, size: Pixels, radius: Pixels, t: &Theme) -> Placeholder {
    let c = t.art_colors_for(name);
    Placeholder {
        bg: c.bg,
        fg: c.fg,
        border: t.art_placeholder_border,
        size,
        radius,
    }
}

impl gpui::RenderOnce for Placeholder {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(self.size)
            .rounded(self.radius)
            .bg(self.bg)
            .border_1()
            .border_color(self.border)
            .child(icon(
                Icon::AudioLines,
                (self.size * 0.36).max(gpui::px(14.)),
                self.fg,
            ))
    }
}
