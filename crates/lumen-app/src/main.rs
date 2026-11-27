//! Lumen: an offline music player for Windows.

// Release builds are GUI-subsystem apps (no console window).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod albums;
mod app;
mod artists;
mod assets;
mod cover;
mod fonts;
mod library_model;
mod logging;
mod lyrics_view;
mod now_playing;
mod player;
mod player_bar;
mod playlists;
mod popup;
mod settings;
mod sidebar;
mod single_instance;
mod smtc;
mod songs;
mod text_input;
mod theme;
mod titlebar;
mod ui;
mod widgets;

use gpui::{
    App, AppContext, Application, Bounds, SharedString, TextRun, TitlebarOptions, WindowBounds,
    WindowOptions, px, size,
};

use crate::app::Root;
use crate::assets::Assets;
use crate::theme::Theme;

fn main() {
    let font_check = std::env::args().any(|a| a == "--font-check");

    // One Lumen per library: a second launch focuses the running window.
    let data_dir = std::env::var_os("LUMEN_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(lumen_library::data_dir);
    if !font_check && !single_instance::acquire(&data_dir) {
        return;
    }
    logging::init(&data_dir);

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            if let Err(err) = fonts::register(cx) {
                eprintln!("lumen: failed to register bundled fonts: {err:#}");
            }
            cx.set_global(Theme::night());
            app::bind_keys(cx);
            text_input::bind_keys(cx);

            // Quit when the last window closes.
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Lumen".into()),
                    // Hides the system caption; we draw our own title bar.
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_min_size: Some(size(px(960.), px(600.))),
                app_id: Some("Lumen".into()),
                ..Default::default()
            };

            let opened = cx.open_window(options, |window, cx| {
                if font_check {
                    report_font_resolution(window);
                }
                cx.new(|cx| Root::new(window, cx))
            });
            match opened {
                Ok(_) if font_check => cx.quit(),
                Ok(_) => {}
                Err(err) => {
                    eprintln!("lumen: failed to open the main window: {err:#}");
                    cx.quit();
                }
            }
        });
}

/// `lumen --font-check`: prints which font family actually shaped each test
/// string, proving the bundled Noto fallbacks (not system fonts) are used.
fn report_font_resolution(window: &mut gpui::Window) {
    use gpui::FontWeight;
    let samples = [
        (fonts::UI, FontWeight::NORMAL, "Hindi", "संगीत की दुनिया"),
        (
            fonts::UI,
            FontWeight::NORMAL,
            "Malayalam",
            "സംഗീതത്തിന്റെ ലോകം",
        ),
        (fonts::UI, FontWeight::SEMIBOLD, "Hindi", "संगीत की दुनिया"),
        (fonts::UI, FontWeight::NORMAL, "Latin", "canción añoranza"),
        (fonts::DISPLAY, FontWeight::BOLD, "Hindi", "रात की धुन"),
        (
            fonts::DISPLAY,
            FontWeight::SEMIBOLD,
            "Malayalam",
            "രാത്രിയുടെ ഈണം",
        ),
    ];
    let text_system = window.text_system().clone();

    // Resolve the candidate fallback faces up front so their FontIds are known;
    // a fallback face picked during shaping then maps back to the same id.
    let mut known: Vec<(gpui::FontId, String)> = Vec::new();
    for family in ["Noto Sans Devanagari", "Noto Sans Malayalam", "Nirmala UI"] {
        for weight in [
            FontWeight::NORMAL,
            FontWeight::MEDIUM,
            FontWeight::SEMIBOLD,
            FontWeight::BOLD,
        ] {
            let mut font = gpui::font(family);
            font.weight = weight;
            let id = text_system.resolve_font(&font);
            known.push((id, format!("{family} {}", weight.0)));
        }
    }

    for (family, weight, label, text) in samples {
        let mut font = gpui::font(family);
        font.weight = weight;
        font.fallbacks = Some(fonts::fallbacks());
        let run = TextRun {
            len: text.len(),
            font,
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = text_system.shape_line(SharedString::from(text), px(14.), &[run], None);
        let mut used: Vec<String> = Vec::new();
        for shaped in &line.runs {
            let name = known
                .iter()
                .find(|(id, _)| *id == shaped.font_id)
                .map(|(_, name)| name.clone())
                .or_else(|| {
                    text_system
                        .get_font_for_id(shaped.font_id)
                        .map(|f| f.family.to_string())
                })
                .unwrap_or_else(|| format!("unknown {:?}", shaped.font_id));
            if !used.contains(&name) {
                used.push(name);
            }
        }
        println!(
            "{family:>13} {:>3} / {label:<9} -> {}",
            weight.0,
            used.join(", ")
        );
    }
}
