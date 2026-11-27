//! The window's views and the app state behind the library screens.
//!
//! - [`Root`]: title bar, the main area and the player bar (the mini player,
//!   or the transport bar on Now Playing), or the full-screen view. Position
//!   ticks only redraw the player bar: the main area is a cached view.
//! - [`LumenApp`]: sidebar, screens, sorting, search, popups and playback
//!   requests.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::StatefulInteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyView, App, AppContext, Context, ElementId, Entity, FocusHandle, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, PathPromptOptions, Pixels, Point, Render, ScrollHandle,
    StyleRefinement, Styled, UniformListScrollHandle, Window, actions, div, px,
};
use lumen_core::{
    AlbumSort, ArtistSort, ResumeState, Settings, SongFilter, SongSort, SortDir, Track, TrackId,
};
use lumen_library::{LibraryEvent, ListStats};

use crate::assets::Icon;
use crate::cover::CoverCache;
use crate::library_model::LibraryModel;
use crate::lyrics_view::{LyricsMode, LyricsView};
use crate::player::{Player, PlayerEvent};
use crate::player_bar::{BarMode, PlayerBar};
use crate::text_input::{InputEvent, TextInput};
use crate::theme::Theme;
use crate::ui::FontExt;
use crate::{albums, artists, now_playing, playlists, popup, settings, sidebar, songs, titlebar};

actions!(
    lumen,
    [
        /// Ctrl+F: focus the sidebar search.
        FocusSearch,
        /// Enter in a song table: play the selected song.
        PlaySelected,
        SelectNext,
        SelectPrev,
        /// Esc: close a popup, go back from a detail view, or leave full screen.
        Back,
        /// Space.
        TogglePlay,
        /// Ctrl+→ / Ctrl+←.
        NextTrack,
        PrevTrack,
        /// → / ←: seek 5 s.
        SeekForward,
        SeekBack,
        /// Ctrl+↑ / Ctrl+↓.
        VolumeUp,
        VolumeDown,
        /// Ctrl+1…5.
        ShowNowPlaying,
        ShowSongs,
        ShowAlbums,
        ShowArtists,
        ShowPlaylists,
        /// F11: full-screen lyrics.
        ToggleFullScreen,
    ]
);

/// Key context of the song tables.
pub const SONG_TABLE: &str = "SongTable";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-f", FocusSearch, None),
        KeyBinding::new("escape", Back, None),
        KeyBinding::new("space", TogglePlay, None),
        KeyBinding::new("ctrl-right", NextTrack, None),
        KeyBinding::new("ctrl-left", PrevTrack, None),
        KeyBinding::new("right", SeekForward, None),
        KeyBinding::new("left", SeekBack, None),
        KeyBinding::new("ctrl-up", VolumeUp, None),
        KeyBinding::new("ctrl-down", VolumeDown, None),
        KeyBinding::new("ctrl-1", ShowNowPlaying, None),
        KeyBinding::new("ctrl-2", ShowSongs, None),
        KeyBinding::new("ctrl-3", ShowAlbums, None),
        KeyBinding::new("ctrl-4", ShowArtists, None),
        KeyBinding::new("ctrl-5", ShowPlaylists, None),
        KeyBinding::new("f11", ToggleFullScreen, None),
        KeyBinding::new("enter", PlaySelected, Some(SONG_TABLE)),
        KeyBinding::new("down", SelectNext, Some(SONG_TABLE)),
        KeyBinding::new("up", SelectPrev, Some(SONG_TABLE)),
    ]);
}

/// The app's top-level screens (sidebar entries).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    NowPlaying,
    Songs,
    Albums,
    Artists,
    Playlists,
    Settings,
}

impl Screen {
    /// Sidebar "LIBRARY" entries, in order. Settings sits at the bottom.
    pub const LIBRARY: [Screen; 5] = [
        Screen::NowPlaying,
        Screen::Songs,
        Screen::Albums,
        Screen::Artists,
        Screen::Playlists,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Screen::NowPlaying => "Now Playing",
            Screen::Songs => "Songs",
            Screen::Albums => "Albums",
            Screen::Artists => "Artists",
            Screen::Playlists => "Playlists",
            Screen::Settings => "Settings",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Screen::NowPlaying => Icon::NowPlaying,
            Screen::Songs => Icon::Songs,
            Screen::Albums => Icon::Albums,
            Screen::Artists => Icon::Artists,
            Screen::Playlists => Icon::Playlists,
            Screen::Settings => Icon::Settings,
        }
    }

    pub fn id(self) -> ElementId {
        ElementId::Name(
            match self {
                Screen::NowPlaying => "nav-now-playing",
                Screen::Songs => "nav-songs",
                Screen::Albums => "nav-albums",
                Screen::Artists => "nav-artists",
                Screen::Playlists => "nav-playlists",
                Screen::Settings => "nav-settings",
            }
            .into(),
        )
    }
}

// ---- root view -------------------------------------------------------------

pub struct Root {
    app: Entity<LumenApp>,
    mini: Entity<PlayerBar>,
    transport: Entity<PlayerBar>,
    fullscreen: Entity<PlayerBar>,
    /// A short message (skipped file, audio error) shown for a few seconds.
    toast: Option<gpui::SharedString>,
    toast_epoch: u64,
}

/// How long a toast stays up.
const TOAST_FOR: Duration = Duration::from_secs(5);

impl Root {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = cx.new(|cx| LumenApp::new(window, cx));
        let (player, covers, thumbs) = {
            let a = app.read(cx);
            (a.player.clone(), a.covers.clone(), a.library.thumbs())
        };
        let full_lyrics = {
            let (player, app) = (player.clone(), app.downgrade());
            cx.new(|cx| LyricsView::new(LyricsMode::FullScreen, player, app, cx))
        };
        let mut bar = |mode, lyrics| {
            let (player, covers, thumbs) = (player.clone(), covers.clone(), thumbs.clone());
            let app = app.downgrade();
            cx.new(|cx| PlayerBar::new(mode, player, app, covers, thumbs, lyrics, cx))
        };
        let mini = bar(BarMode::Mini, None);
        let transport = bar(BarMode::Transport, None);
        let fullscreen = bar(BarMode::FullScreen, Some(full_lyrics));
        let notice = app.update(cx, |a, _| a.library.notice.take());
        // Skipped files and audio errors show as a toast.
        cx.subscribe(&player, |root, player, e: &PlayerEvent, cx| {
            if *e == PlayerEvent::Error
                && let Some(msg) = player.read(cx).message.clone()
            {
                root.show_toast(msg, cx);
            }
        })
        .detach();
        let mut root = Self {
            app,
            mini,
            transport,
            fullscreen,
            toast: None,
            toast_epoch: 0,
        };
        if let Some(msg) = notice {
            root.show_toast_for(msg, Duration::from_secs(15), cx);
        }
        root
    }

    pub fn show_toast(&mut self, msg: impl Into<gpui::SharedString>, cx: &mut Context<Self>) {
        self.show_toast_for(msg, TOAST_FOR, cx);
    }

    pub fn show_toast_for(
        &mut self,
        msg: impl Into<gpui::SharedString>,
        duration: Duration,
        cx: &mut Context<Self>,
    ) {
        self.toast = Some(msg.into());
        self.toast_epoch += 1;
        let epoch = self.toast_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |root, cx| {
                if root.toast_epoch == epoch {
                    root.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::global(cx).clone();
        let (screen, full, root_focus) = {
            let a = self.app.read(cx);
            (a.screen, a.fullscreen, a.root_focus.clone())
        };
        let mut main_style = StyleRefinement::default().flex_1().min_h_0().w_full();
        main_style = main_style.flex().flex_row();

        // Global shortcuts: handled here so they work wherever focus is.
        let app = self.app.clone();
        let on_app = move |f: fn(&mut LumenApp, &mut Window, &mut Context<LumenApp>)| {
            let app = app.clone();
            move |window: &mut Window, cx: &mut App| {
                app.update(cx, |a, cx| f(a, window, cx));
            }
        };
        let player = self.app.read(cx).player.clone();
        let on_player = move |f: fn(&mut Player, &mut Context<Player>)| {
            let player = player.clone();
            move |cx: &mut App| {
                player.update(cx, f);
            }
        };
        let (focus_search, back, full_screen) = (
            on_app(|a, w, cx| a.focus_search(w, cx)),
            on_app(|a, w, cx| a.back(w, cx)),
            on_app(|a, w, cx| {
                let on = !a.fullscreen;
                a.set_fullscreen(on, w, cx)
            }),
        );
        let show = |screen: Screen| {
            let app = self.app.clone();
            move |cx: &mut App| {
                app.update(cx, |a, cx| a.navigate(screen, cx));
            }
        };
        let (s1, s2, s3, s4, s5) = (
            show(Screen::NowPlaying),
            show(Screen::Songs),
            show(Screen::Albums),
            show(Screen::Artists),
            show(Screen::Playlists),
        );
        let toggle = on_player(|p, _| p.toggle());
        let next = on_player(|p, _| p.next());
        let prev = on_player(|p, _| p.prev());
        let fwd = on_player(|p, cx| p.seek_step(true, cx));
        let rew = on_player(|p, cx| p.seek_step(false, cx));
        let vol_up = on_player(|p, cx| p.volume_step(true, cx));
        let vol_down = on_player(|p, cx| p.volume_step(false, cx));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .ui_font()
            .text_size(px(14.))
            .text_color(t.text)
            .on_action(move |_: &FocusSearch, w, cx| focus_search(w, cx))
            .on_action(move |_: &Back, w, cx| back(w, cx))
            .on_action(move |_: &ToggleFullScreen, w, cx| full_screen(w, cx))
            .on_action(move |_: &TogglePlay, _, cx| toggle(cx))
            .on_action(move |_: &NextTrack, _, cx| next(cx))
            .on_action(move |_: &PrevTrack, _, cx| prev(cx))
            .on_action(move |_: &SeekForward, _, cx| fwd(cx))
            .on_action(move |_: &SeekBack, _, cx| rew(cx))
            .on_action(move |_: &VolumeUp, _, cx| vol_up(cx))
            .on_action(move |_: &VolumeDown, _, cx| vol_down(cx))
            .on_action(move |_: &ShowNowPlaying, _, cx| s1(cx))
            .on_action(move |_: &ShowSongs, _, cx| s2(cx))
            .on_action(move |_: &ShowAlbums, _, cx| s3(cx))
            .on_action(move |_: &ShowArtists, _, cx| s4(cx))
            .on_action(move |_: &ShowPlaylists, _, cx| s5(cx))
            .relative()
            .map(|d| {
                // Keyboard focus rests on `root_focus` when no table or input
                // has it, so the shortcuts above always receive keys. It sits
                // below the title bar on purpose: a focus target claims every
                // mouse press inside it, and on the title bar that would
                // swallow drags and the minimize / maximize / close clicks.
                let body = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .track_focus(&root_focus);
                if full {
                    // Full-screen lyrics: no title bar, sidebar or mini player.
                    d.child(body.child(self.fullscreen.clone()))
                } else {
                    d.child(titlebar::render(window, &t)).child(
                        body.child(AnyView::from(self.app.clone()).cached(main_style))
                            .child(if screen == Screen::NowPlaying {
                                self.transport.clone()
                            } else {
                                self.mini.clone()
                            }),
                    )
                }
            })
            .when_some(self.toast.clone(), |d, msg| {
                d.child(
                    div()
                        .id("toast")
                        .absolute()
                        .right(px(20.))
                        .bottom(px(96.))
                        .max_w(px(420.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .px(px(14.))
                        .py(px(10.))
                        .rounded(px(10.))
                        .bg(t.surface_2)
                        .border_1()
                        .border_color(t.border_input)
                        .shadow_lg()
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .text_color(t.text)
                        .cursor_pointer()
                        .on_click(cx.listener(|root, _, _, cx| {
                            root.toast = None;
                            cx.notify();
                        }))
                        .child(div().size(px(8.)).flex_none().rounded_full().bg(t.accent))
                        .child(div().min_w_0().child(msg)),
                )
            })
    }
}

// ---- main area -------------------------------------------------------------

/// Songs screen state.
pub struct SongsState {
    pub filter: SongFilter,
    /// Catalog indices in display order.
    pub rows: Arc<Vec<usize>>,
    pub stats: ListStats,
    pub selected: Option<TrackId>,
    pub scroll: UniformListScrollHandle,
}

/// Albums screen state.
pub struct AlbumsState {
    /// Album indices in display order.
    pub rows: Arc<Vec<usize>>,
    /// Album detail view (album id).
    pub open: Option<i64>,
    pub scroll: UniformListScrollHandle,
    pub detail_scroll: ScrollHandle,
}

/// Artists screen state. Artist → album is a two-level stack.
pub struct ArtistsState {
    pub rows: Arc<Vec<usize>>,
    pub open: Option<i64>,
    pub open_album: Option<i64>,
    pub scroll: UniformListScrollHandle,
    pub detail_scroll: ScrollHandle,
    pub album_scroll: ScrollHandle,
}

/// The open popup, if any.
#[derive(Clone, Debug, PartialEq)]
pub enum Popup {
    SongSort,
    AlbumSort,
    ArtistSort,
    SettingsSongSort,
    SettingsSongDir,
    SettingsLyricsFont,
    SettingsDevice,
    SettingsRate,
    PlaylistAdd,
    PlaylistMenu,
    PlaylistSort,
    /// Right-click menu on a song. `entry` = (playlist, position) when the
    /// song was clicked inside a user playlist.
    Song {
        track: TrackId,
        at: Point<Pixels>,
        submenu: bool,
        entry: Option<(i64, usize)>,
    },
    /// Right-click menu on an album card.
    Album {
        id: i64,
        at: Point<Pixels>,
    },
    /// Right-click menu on an artist card.
    Artist {
        id: i64,
        at: Point<Pixels>,
    },
    /// "Edit lyrics offset" for a song.
    LyricsOffset {
        track: TrackId,
        at: Point<Pixels>,
        offset_ms: i32,
    },
}

pub struct LumenApp {
    pub screen: Screen,
    pub library: LibraryModel,
    pub player: Entity<Player>,
    pub covers: Entity<CoverCache>,
    pub search: Entity<TextInput>,
    pub query: String,
    pub settings: Settings,
    settings_file: PathBuf,
    pub songs: SongsState,
    pub albums: AlbumsState,
    pub artists: ArtistsState,
    pub popup: Option<Popup>,
    /// Keyboard focus for the song tables (Enter, arrows).
    pub table_focus: FocusHandle,
    /// Focus for the window as a whole (shortcuts) when no table is shown.
    pub root_focus: FocusHandle,
    /// Track id of the audible track, mirrored from the player.
    pub playing: Option<TrackId>,
    /// Full-screen lyrics view (F11).
    pub fullscreen: bool,
    /// The lyrics panel on Now Playing.
    pub lyrics_panel: Entity<LyricsView>,
    /// "Reset play counts" is waiting for confirmation.
    pub confirm_reset: bool,
    pub playlists: crate::playlists::PlaylistsState,
    /// On-device recommendations (`reco.rs`).
    pub reco: crate::reco::RecoState,
    /// A playlist the library just created; handled at the next render
    /// (which has the window, for the rename field).
    pending_created: Option<(i64, usize, usize)>,
}

impl LumenApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let started = std::time::Instant::now();
        let settings_dir = std::env::var_os("LUMEN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(Settings::default_dir);
        let settings_file = Settings::file_in(&settings_dir);
        let settings = Settings::load(&settings_file);
        let library = LibraryModel::open(&settings.library);
        crate::ui::profile_log("open library and read catalog", started);
        if let Some(lib) = library.library() {
            Self::bridge_library_events(lib.events().clone(), cx);
        }
        // Keep "Last scan 2 min ago" current. One wake-up every 30 s.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();

        let lib_handle = library.library().cloned();
        let data_dir = library
            .paths
            .db
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(lumen_library::data_dir);
        let thumbs = library.thumbs();
        let player =
            cx.new(|cx| Player::new(lib_handle, data_dir.clone(), thumbs, &settings.audio, cx));
        cx.subscribe(&player, |this, player, e: &PlayerEvent, cx| {
            if *e == PlayerEvent::Changed {
                this.reco_player_changed(cx);
                let now = player.read(cx).current_id();
                if now != this.playing {
                    this.sync_lyrics_offset(cx);
                }
                // Now Playing shows state, badges and Up Next; the other
                // screens only mark the playing row.
                if now != this.playing
                    || matches!(this.screen, Screen::NowPlaying | Screen::Settings)
                {
                    this.playing = now;
                    cx.notify();
                }
            }
        })
        .detach();
        if let Some(hwnd) = window_handle(window) {
            player.update(cx, |p, cx| p.attach_smtc(hwnd, cx));
        }
        player.update(cx, |p, cx| p.set_lyrics_prefs(settings.lyrics.clone(), cx));
        Self::resume(&library, &data_dir, &player, cx);
        let lyrics_panel = {
            let (player, app) = (player.clone(), cx.entity().downgrade());
            cx.new(|cx| LyricsView::new(LyricsMode::Panel, player, app, cx))
        };

        let search = cx.new(|cx| TextInput::new("Search library", cx));
        cx.subscribe_in(&search, window, |this, _, e: &InputEvent, window, cx| {
            this.on_search(e, window, cx);
        })
        .detach();

        let covers = cx.new(CoverCache::new);
        let table_focus = cx.focus_handle();
        window.focus(&table_focus);
        let root_focus = cx.focus_handle();

        let mut app = Self {
            screen: Screen::Songs,
            library,
            player,
            covers,
            search,
            query: String::new(),
            settings,
            settings_file,
            songs: SongsState {
                filter: SongFilter::All,
                rows: Arc::default(),
                stats: ListStats::default(),
                selected: None,
                scroll: UniformListScrollHandle::new(),
            },
            albums: AlbumsState {
                rows: Arc::default(),
                open: None,
                scroll: UniformListScrollHandle::new(),
                detail_scroll: ScrollHandle::new(),
            },
            artists: ArtistsState {
                rows: Arc::default(),
                open: None,
                open_album: None,
                scroll: UniformListScrollHandle::new(),
                detail_scroll: ScrollHandle::new(),
                album_scroll: ScrollHandle::new(),
            },
            popup: None,
            table_focus,
            root_focus,
            playing: None,
            fullscreen: false,
            lyrics_panel,
            confirm_reset: false,
            playlists: Default::default(),
            reco: Default::default(),
            pending_created: None,
        };
        app.recompute();
        app.open_reco(cx);
        app
    }

    /// Loads the last session's queue, paused where it stopped.
    fn resume(
        library: &LibraryModel,
        data_dir: &std::path::Path,
        player: &Entity<Player>,
        cx: &mut Context<Self>,
    ) {
        let state = ResumeState::load(&ResumeState::file_in(data_dir));
        let c = &library.catalog;
        let mut tracks = Vec::new();
        let mut start = None;
        let mut before_current = 0;
        for (i, id) in state.queue.iter().enumerate() {
            if let Some(ix) = c.index_of(TrackId(*id)) {
                if i == state.current {
                    start = Some(tracks.len());
                }
                if i < state.current {
                    before_current += 1;
                }
                tracks.push(c.track(ix).clone());
            }
        }
        // The track that was playing is gone: start at the one after it.
        let (start, position) = match start {
            Some(s) => (s, Duration::from_millis(state.position_ms)),
            None => (
                before_current.min(tracks.len().saturating_sub(1)),
                Duration::ZERO,
            ),
        };
        player.update(cx, |p, cx| p.restore(tracks, start, position, &state, cx));
    }

    /// The playing track's lyrics offset may have changed in the library
    /// (context menu, another track's snapshot): hand the current value to
    /// the player.
    fn sync_lyrics_offset(&self, cx: &mut Context<Self>) {
        let id = self.player.read(cx).current_id();
        let c = &self.library.catalog;
        if let Some(ms) = id
            .and_then(|id| c.index_of(id))
            .map(|i| c.track(i).lyrics_offset_ms)
        {
            self.player.update(cx, |p, cx| p.sync_lyrics_offset(ms, cx));
        }
    }

    /// Applies a change to the lyrics settings and saves it.
    pub fn update_lyrics_settings(
        &mut self,
        f: impl FnOnce(&mut lumen_core::LyricsSettings),
        cx: &mut Context<Self>,
    ) {
        f(&mut self.settings.lyrics);
        self.save_settings();
        let prefs = self.settings.lyrics.clone();
        self.player
            .update(cx, |p, cx| p.set_lyrics_prefs(prefs, cx));
        cx.notify();
    }

    /// Applies a change to Settings › Audio, saves it and tells the engine.
    pub fn update_audio_settings(
        &mut self,
        f: impl FnOnce(&mut lumen_core::AudioSettings),
        cx: &mut Context<Self>,
    ) {
        f(&mut self.settings.audio);
        self.save_settings();
        let audio = self.settings.audio.clone();
        self.player.update(cx, |p, _| p.apply_audio(&audio));
        cx.notify();
    }

    /// "Watch folders for changes" and "Track file date modified".
    pub fn update_library_settings(
        &mut self,
        f: impl FnOnce(&mut lumen_core::LibrarySettings),
        cx: &mut Context<Self>,
    ) {
        f(&mut self.settings.library);
        self.save_settings();
        if let Some(l) = self.library.library() {
            l.set_watch(self.settings.library.watch);
            l.set_track_mtime(self.settings.library.track_mtime);
        }
        cx.notify();
    }

    /// "Reset play counts", after the confirm step.
    pub fn reset_play_counts(&mut self, cx: &mut Context<Self>) {
        if let Some(l) = self.library.library() {
            l.reset_play_counts();
        }
        self.confirm_reset = false;
        cx.notify();
    }

    /// A− / A+ in full-screen lyrics (persisted).
    pub fn step_lyrics_scale(&mut self, up: bool, cx: &mut Context<Self>) {
        self.update_lyrics_settings(|l| l.step_scale(up), cx);
    }

    /// Enters or leaves the borderless full-screen lyrics view.
    pub fn set_fullscreen(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.fullscreen == on {
            return;
        }
        self.fullscreen = on;
        self.popup = None;
        window.focus(&self.root_focus);
        if window.is_fullscreen() != on {
            window.toggle_fullscreen();
        }
        cx.notify();
    }

    /// Forwards library events (a blocking channel) into the UI's async
    /// runtime, coalescing bursts into one redraw.
    fn bridge_library_events(events: lumen_library::EventReceiver, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<LibraryEvent>();
        let spawned = std::thread::Builder::new()
            .name("lumen-library-events".into())
            .spawn(move || {
                for e in events.iter() {
                    if tx.unbounded_send(e).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!("cannot forward library events: {e}");
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while let Ok(e) = rx.try_recv() {
                    batch.push(e);
                }
                let alive = this.update(cx, |app, cx| {
                    let created: Vec<(i64, usize, usize)> = batch
                        .iter()
                        .filter_map(|e| match e {
                            LibraryEvent::PlaylistCreated { id, added, missing } => {
                                Some((*id, *added, *missing))
                            }
                            _ => None,
                        })
                        .collect();
                    let scan_finished = batch
                        .iter()
                        .any(|e| matches!(e, LibraryEvent::ScanFinished { .. }));
                    let changed = batch
                        .iter()
                        .any(|e| matches!(e, LibraryEvent::Changed | LibraryEvent::FoldersChanged));
                    let applied = app.library.apply(batch);
                    app.reco_library_events(scan_finished, changed);
                    for (id, added, missing) in created {
                        app.pending_created = Some((id, added, missing));
                    }
                    if applied.reload {
                        app.reload_catalog(cx);
                    }
                    if applied.redraw {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Re-reads the catalog on a background thread. Overlapping requests
    /// collapse into one more read after the current one.
    fn reload_catalog(&mut self, cx: &mut Context<Self>) {
        if self.library.loading {
            self.library.stale = true;
            return;
        }
        self.library.loading = true;
        let db = self.library.paths.db.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { lumen_library::Reader::open(&db).and_then(|r| r.catalog()) })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.library.loading = false;
                match result {
                    Ok(c) => {
                        app.library.catalog = Arc::new(c);
                        app.sync_lyrics_offset(cx);
                        app.reco_catalog_changed(cx);
                        let started = std::time::Instant::now();
                        app.recompute();
                        crate::ui::profile_log("sort and filter all screens", started);
                        cx.notify();
                    }
                    Err(e) => tracing::warn!("cannot reload the library: {e}"),
                }
                if std::mem::take(&mut app.library.stale) {
                    app.reload_catalog(cx);
                }
            });
        })
        .detach();
    }

    /// Recomputes every screen's list from the catalog, sort and query.
    pub fn recompute(&mut self) {
        self.recompute_songs();
        self.recompute_albums();
        self.recompute_artists();
        self.recompute_playlist();
    }

    pub fn recompute_songs(&mut self) {
        let c = &self.library.catalog;
        let l = &self.settings.library;
        let rows = c.songs(l.song_sort, l.song_sort_dir, self.songs.filter, &self.query);
        self.songs.stats = c.stats(&rows);
        self.songs.rows = Arc::new(rows);
    }

    pub fn recompute_albums(&mut self) {
        let l = &self.settings.library;
        self.albums.rows = Arc::new(self.library.catalog.album_list(
            l.album_sort,
            l.album_sort_dir,
            &self.query,
        ));
        if let Some(id) = self.albums.open
            && self.album_index(id).is_none()
        {
            self.albums.open = None;
        }
    }

    pub fn recompute_artists(&mut self) {
        let l = &self.settings.library;
        self.artists.rows = Arc::new(self.library.catalog.artist_list(
            l.artist_sort,
            l.artist_sort_dir,
            &self.query,
        ));
        if let Some(id) = self.artists.open
            && self.artist_index(id).is_none()
        {
            self.artists.open = None;
            self.artists.open_album = None;
        }
    }

    pub fn album_index(&self, id: i64) -> Option<usize> {
        self.library.catalog.albums.iter().position(|a| a.id == id)
    }

    pub fn artist_index(&self, id: i64) -> Option<usize> {
        self.library.catalog.artists.iter().position(|a| a.id == id)
    }

    pub(crate) fn save_settings(&self) {
        if let Err(e) = self.settings.save(&self.settings_file) {
            tracing::warn!("cannot save settings: {e}");
        }
    }

    // ---- sorting -----------------------------------------------------------

    pub fn set_song_sort(&mut self, sort: SongSort, dir: SortDir, cx: &mut Context<Self>) {
        let l = &mut self.settings.library;
        if l.song_sort == sort && l.song_sort_dir == dir {
            return;
        }
        l.song_sort = sort;
        l.song_sort_dir = dir;
        self.save_settings();
        self.recompute_songs();
        self.songs
            .scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// Column header click: sort by it, or flip the direction.
    pub fn click_song_column(&mut self, sort: SongSort, cx: &mut Context<Self>) {
        let l = &self.settings.library;
        let dir = if l.song_sort == sort {
            l.song_sort_dir.flip()
        } else {
            sort.natural_dir()
        };
        self.set_song_sort(sort, dir, cx);
    }

    pub fn set_song_filter(&mut self, filter: SongFilter, cx: &mut Context<Self>) {
        if self.songs.filter != filter {
            self.songs.filter = filter;
            self.recompute_songs();
            self.songs
                .scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
            cx.notify();
        }
    }

    pub fn set_album_sort(&mut self, sort: AlbumSort, dir: SortDir, cx: &mut Context<Self>) {
        let l = &mut self.settings.library;
        l.album_sort = sort;
        l.album_sort_dir = dir;
        self.save_settings();
        self.recompute_albums();
        self.albums
            .scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    pub fn set_artist_sort(&mut self, sort: ArtistSort, dir: SortDir, cx: &mut Context<Self>) {
        let l = &mut self.settings.library;
        l.artist_sort = sort;
        l.artist_sort_dir = dir;
        self.save_settings();
        self.recompute_artists();
        self.artists
            .scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    // ---- search ------------------------------------------------------------

    fn on_search(&mut self, e: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match e {
            InputEvent::Changed(text) => {
                let q = text.trim().to_string();
                if q == self.query {
                    return;
                }
                self.query = q;
                let started = std::time::Instant::now();
                // Typing filters the screen you're on: leave detail views.
                self.albums.open = None;
                self.artists.open = None;
                self.artists.open_album = None;
                self.recompute();
                crate::ui::profile_log("search all screens", started);
                self.songs
                    .scroll
                    .scroll_to_item(0, gpui::ScrollStrategy::Top);
                self.albums
                    .scroll
                    .scroll_to_item(0, gpui::ScrollStrategy::Top);
                self.artists
                    .scroll
                    .scroll_to_item(0, gpui::ScrollStrategy::Top);
                cx.notify();
            }
            InputEvent::Submit => {
                // Enter: Songs, filtered by the query.
                self.navigate(Screen::Songs, cx);
                if self.songs.selected.is_none()
                    && let Some(&first) = self.songs.rows.first()
                {
                    self.songs.selected = Some(self.library.catalog.track(first).id);
                }
                window.focus(&self.table_focus);
                cx.notify();
            }
            InputEvent::Cancel => {
                if self.search.read(cx).text().is_empty() {
                    window.focus(&self.table_focus);
                } else {
                    self.search.update(cx, |s, cx| s.set_text("", cx));
                }
            }
        }
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |s, cx| s.focus_and_select_all(window, cx));
    }

    /// Esc: close the popup, else step back out of a detail view.
    pub fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.fullscreen {
            self.set_fullscreen(false, window, cx);
            return;
        }
        if self.popup.take().is_some() {
        } else if self.screen == Screen::Albums && self.albums.open.is_some() {
            self.albums.open = None;
        } else if self.screen == Screen::Artists && self.artists.open_album.is_some() {
            self.artists.open_album = None;
        } else if self.screen == Screen::Artists && self.artists.open.is_some() {
            self.artists.open = None;
        } else {
            return;
        }
        cx.notify();
    }

    // ---- navigation --------------------------------------------------------

    pub fn navigate(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.popup = None;
        self.confirm_reset = false;
        self.reco.confirm_reset = false;
        if screen == Screen::Settings {
            self.player.read(cx).refresh_devices();
        }
        if self.screen != screen {
            self.screen = screen;
            cx.notify();
        } else if screen == Screen::Albums && self.albums.open.is_some() {
            // Clicking the active nav item returns to the grid.
            self.albums.open = None;
            cx.notify();
        } else if screen == Screen::Artists && self.artists.open.is_some() {
            self.artists.open = None;
            self.artists.open_album = None;
            cx.notify();
        }
    }

    pub fn open_album(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.screen == Screen::Artists {
            self.artists.open_album = Some(id);
            self.artists.album_scroll = ScrollHandle::new();
        } else {
            self.screen = Screen::Albums;
            self.albums.open = Some(id);
            self.albums.detail_scroll = ScrollHandle::new();
        }
        cx.notify();
    }

    pub fn open_artist(&mut self, id: i64, cx: &mut Context<Self>) {
        self.screen = Screen::Artists;
        self.artists.open = Some(id);
        self.artists.open_album = None;
        self.artists.detail_scroll = ScrollHandle::new();
        cx.notify();
    }

    // ---- playback ----------------------------------------------------------

    /// Plays catalog rows `list` starting at position `start`. With
    /// `shuffle`, turns shuffle on and starts anywhere.
    pub fn play_rows(
        &mut self,
        list: &[usize],
        start: usize,
        shuffle: bool,
        cx: &mut Context<Self>,
    ) {
        let c = self.library.catalog.clone();
        let tracks: Vec<Track> = list.iter().map(|&i| c.track(i).clone()).collect();
        if tracks.is_empty() {
            return;
        }
        let start = if shuffle {
            // Any start; the engine shuffles the rest.
            (crate::ui::now_ms() as usize) % tracks.len()
        } else {
            start.min(tracks.len() - 1)
        };
        self.player.update(cx, |p, cx| {
            // Shuffle buttons turn shuffle on; everything else keeps the
            // transport toggle as it is.
            if shuffle {
                p.set_shuffle(true, cx);
            }
            p.play_list(tracks, start, cx);
        });
    }

    /// "Play next" / "Add to queue" for one track.
    pub fn enqueue(&mut self, id: TrackId, next: bool, cx: &mut Context<Self>) {
        let c = &self.library.catalog;
        if let Some(i) = c.index_of(id) {
            let t = c.track(i).clone();
            self.player.update(cx, |p, cx| p.enqueue(vec![t], next, cx));
        }
    }

    // ---- library actions ---------------------------------------------------

    pub fn add_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add to library".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update(cx, |app, cx| {
                    for p in paths {
                        app.library.add_folder(p);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub fn remove_folder(&mut self, id: i64, cx: &mut Context<Self>) {
        self.library.remove_folder(id);
        cx.notify();
    }

    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.library.rescan();
        cx.notify();
    }

    pub fn open_popup(&mut self, popup: Popup, cx: &mut Context<Self>) {
        self.popup = if self.popup.as_ref() == Some(&popup) {
            None
        } else {
            Some(popup)
        };
        cx.notify();
    }

    pub fn close_popup(&mut self, cx: &mut Context<Self>) {
        if self.popup.take().is_some() {
            cx.notify();
        }
    }
}

impl Render for LumenApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        let t = Theme::global(cx).clone();
        // The focused table left the screen: hand focus back to the window
        // so shortcuts keep working.
        let has_table = match self.screen {
            Screen::Songs => !self.songs.rows.is_empty(),
            Screen::Albums => self.albums.open.is_some(),
            Screen::Artists => self.artists.open.is_some() || self.artists.open_album.is_some(),
            _ => false,
        };
        if !has_table && self.table_focus.is_focused(window) {
            cx.defer_in(window, |this, window, _| window.focus(&this.root_focus));
        }
        if let Some((id, added, missing)) = self.pending_created.take() {
            // The playlist list is already refreshed (same event batch).
            self.on_playlist_created(id, added, missing, Some(window), cx);
        }
        let content = match self.screen {
            Screen::Settings => settings::render(self, &t, cx).into_any_element(),
            Screen::Songs => songs::render(self, &t, window, cx).into_any_element(),
            Screen::Albums => albums::render(self, &t, window, cx).into_any_element(),
            Screen::Artists => artists::render(self, &t, window, cx).into_any_element(),
            Screen::NowPlaying => now_playing::render(self, &t, window, cx).into_any_element(),
            Screen::Playlists => playlists::render(self, &t, window, cx).into_any_element(),
        };
        crate::ui::profile_log("main view render", started);
        div()
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .image_cache(self.covers.clone())
            .child(sidebar::render(self, &t, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .child(content),
            )
            .children(popup::render(self, &t, window, cx))
    }
}

/// The main window's Win32 handle (for the media controls).
fn window_handle(window: &Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}
