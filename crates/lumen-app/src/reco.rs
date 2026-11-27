//! The app side of on-device recommendations: opening `lumen-reco`, the
//! "Made for you" mixes, Radio, Play similar and More like this.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::Context;
use lumen_core::{RecoSettings, Track, TrackId};
use lumen_reco::{AnalysisStatus, Mix, MixKind, Reco, RecoEvent, Snapshot};

use crate::app::{LumenApp, Screen};
use crate::playlists::PlaylistSel;
use crate::ui::now_ms;

/// Radio adds this many tracks, seeded from the last `RADIO_SEEDS` played.
const RADIO_ADD: usize = 10;
const RADIO_SEEDS: usize = 3;
/// "Play similar": the item plus this many.
const SIMILAR: usize = 24;
/// "MORE LIKE THIS" on Now Playing.
const MORE_LIKE_THIS: usize = 5;

#[derive(Default)]
pub struct RecoState {
    pub handle: Option<Arc<Reco>>,
    /// The catalog as scoring sees it.
    pub snap: Arc<Snapshot>,
    pub mixes: Vec<Mix>,
    /// Bumped when vectors or history change (invalidates More like this).
    pub version: u64,
    /// (track, version) → its More like this list.
    more_like: Option<(TrackId, u64, Vec<TrackId>)>,
    /// Recently started tracks, newest last (Radio's seeds).
    recent: VecDeque<TrackId>,
    /// Radio already extended the queue at (track, queue length).
    radio_done: Option<(TrackId, usize)>,
    mixes_loading: bool,
    /// "Reset listening history" is waiting for confirmation.
    pub confirm_reset: bool,
}

impl RecoState {
    pub fn status(&self) -> Option<AnalysisStatus> {
        self.handle.as_ref().map(|r| r.status())
    }

    pub fn mix(&self, kind: MixKind) -> Option<&Mix> {
        self.mixes.iter().find(|m| m.kind == kind)
    }
}

impl LumenApp {
    pub fn reco_on(&self) -> bool {
        self.settings.reco.enabled && self.reco.handle.is_some()
    }

    /// Opens the recommendation system on the library database and starts
    /// listening to it. Analysis begins after the library's startup scan.
    pub fn open_reco(&mut self, cx: &mut Context<Self>) {
        if self.library.library().is_none() {
            return;
        }
        let started = std::time::Instant::now();
        match Reco::open(&self.library.paths.db, self.settings.reco.enabled) {
            Ok(r) => {
                let r = Arc::new(r);
                Self::bridge_reco_events(r.events().clone(), cx);
                self.player.update(cx, |p, _| p.set_reco(Some(r.clone())));
                self.reco.handle = Some(r);
            }
            Err(e) => tracing::warn!("recommendations unavailable: {e}"),
        }
        self.reco.snap = Arc::new(Snapshot::from_catalog(&self.library.catalog));
        crate::ui::profile_log("open recommendations", started);
        self.refresh_mixes(false, cx);
        // "At most once every 6 hours": check hourly whether a mix is due.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(3600))
                    .await;
                if this.update(cx, |a, cx| a.refresh_mixes(false, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Forwards recommendation events into the UI, at most a few redraws a
    /// second while analysis runs.
    fn bridge_reco_events(events: crossbeam_channel::Receiver<RecoEvent>, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<RecoEvent>();
        let spawned = std::thread::Builder::new()
            .name("lumen-reco-events".into())
            .spawn(move || {
                for e in events.iter() {
                    if tx.unbounded_send(e).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!("cannot forward recommendation events: {e}");
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let mut batch = vec![first];
                while let Ok(e) = rx.try_recv() {
                    batch.push(e);
                }
                let alive = this.update(cx, |app, cx| {
                    let mut finished = false;
                    for e in &batch {
                        match e {
                            RecoEvent::ModelChanged => app.reco.version += 1,
                            RecoEvent::RunFinished {
                                analyzed,
                                failed,
                                elapsed,
                            } => {
                                tracing::info!(
                                    "sound analysis: {analyzed} tracks in {:.1} s, {failed} failed",
                                    elapsed.as_secs_f64()
                                );
                                finished = true;
                            }
                            RecoEvent::Analysis => {}
                        }
                    }
                    if finished {
                        app.refresh_mixes(false, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Library events: analyze after a scan (retrying failures after a
    /// full rescan), and pick up single file changes while idle.
    pub fn reco_library_events(&mut self, scan_finished: bool, changed: bool) {
        let Some(r) = &self.reco.handle else { return };
        if self.library.scan.is_some() {
            return;
        }
        if scan_finished {
            r.rescan();
        } else if changed {
            r.refresh();
        }
    }

    /// The catalog was re-read: rebuild the scoring snapshot.
    pub fn reco_catalog_changed(&mut self, cx: &mut Context<Self>) {
        self.reco.snap = Arc::new(Snapshot::from_catalog(&self.library.catalog));
        self.reco.more_like = None;
        if self.reco.mixes.iter().all(|m| m.tracks.is_empty()) {
            self.refresh_mixes(false, cx);
        }
    }

    /// Loads the mixes, rebuilding those older than 6 hours (or empty) on a
    /// background thread.
    pub fn refresh_mixes(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.reco_on() || self.reco.mixes_loading {
            return;
        }
        let Some(r) = self.reco.handle.clone() else {
            return;
        };
        let snap = self.reco.snap.clone();
        let skips = self.settings.reco.skips_as_dislikes;
        self.reco.mixes_loading = true;
        cx.spawn(async move |this, cx| {
            let mixes = cx
                .background_executor()
                .spawn(async move { r.mixes(&snap, now_ms(), skips, force) })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.reco.mixes_loading = false;
                app.reco.mixes = mixes;
                if matches!(app.playlists.selected, PlaylistSel::Mix(_)) {
                    app.recompute_playlist();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings › Recommendations changes.
    pub fn update_reco_settings(
        &mut self,
        f: impl FnOnce(&mut RecoSettings),
        cx: &mut Context<Self>,
    ) {
        let was = self.settings.reco.enabled;
        f(&mut self.settings.reco);
        self.save_settings();
        let on = self.settings.reco.enabled;
        if let Some(r) = &self.reco.handle
            && on != was
        {
            r.set_enabled(on);
        }
        if !on && matches!(self.playlists.selected, PlaylistSel::Mix(_)) {
            self.playlists.selected = PlaylistSel::Smart(lumen_library::SmartPlaylist::HiRes);
            self.recompute_playlist();
        }
        if on && !was {
            self.refresh_mixes(false, cx);
        }
        self.maybe_extend_radio(cx);
        cx.notify();
    }

    pub fn toggle_radio(&mut self, cx: &mut Context<Self>) {
        self.update_reco_settings(|r| r.radio = !r.radio, cx);
    }

    /// "Reset listening history", after the confirm step.
    pub fn reset_listening_history(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = &self.reco.handle {
            r.reset_history();
        }
        self.reco.confirm_reset = false;
        self.reco.version += 1;
        self.reco.more_like = None;
        self.refresh_mixes(true, cx);
        cx.notify();
    }

    pub fn reanalyze_library(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = &self.reco.handle {
            r.reanalyze();
        }
        cx.notify();
    }

    /// Opens a mix from the sidebar.
    pub fn open_mix(&mut self, kind: MixKind, cx: &mut Context<Self>) {
        self.navigate(Screen::Playlists, cx);
        self.select_playlist(PlaylistSel::Mix(kind), cx);
    }

    /// "Save as playlist": an editable copy of a mix.
    pub fn save_mix_as_playlist(&mut self, kind: MixKind, cx: &mut Context<Self>) {
        let (Some(l), Some(mix)) = (self.library.library(), self.reco.mix(kind)) else {
            return;
        };
        let name = format!("{} · {}", kind.name(), crate::ui::format_date(now_ms()));
        self.playlists.select_next_created = true;
        self.playlists.rename_next_created = false;
        l.new_playlist(name, mix.tracks.clone());
        cx.notify();
    }

    fn tracks_of(&self, ids: &[TrackId]) -> Vec<Track> {
        let c = &self.library.catalog;
        ids.iter()
            .filter_map(|id| c.index_of(*id).map(|i| c.track(i).clone()))
            .collect()
    }

    fn recommend(
        &self,
        seeds: &[TrackId],
        count: usize,
        exclude: &HashSet<TrackId>,
    ) -> Vec<TrackId> {
        let Some(r) = &self.reco.handle else {
            return Vec::new();
        };
        r.recommend(
            &self.reco.snap,
            seeds,
            count,
            exclude,
            now_ms(),
            self.settings.reco.skips_as_dislikes,
        )
    }

    /// "Play similar": `start`, then 24 tracks like `seeds`, as a new queue.
    pub fn play_similar(&mut self, seeds: Vec<TrackId>, start: TrackId, cx: &mut Context<Self>) {
        if !self.reco_on() {
            return;
        }
        let exclude: HashSet<TrackId> = [start].into();
        let mut ids = vec![start];
        ids.extend(self.recommend(&seeds, SIMILAR, &exclude));
        let tracks = self.tracks_of(&ids);
        self.popup = None;
        self.player.update(cx, |p, cx| p.play_list(tracks, 0, cx));
        cx.notify();
    }

    /// Play similar for an album: its first track, then music like the album.
    pub fn play_similar_album(&mut self, album_id: i64, cx: &mut Context<Self>) {
        let Some(ix) = self.album_index(album_id) else {
            return;
        };
        let c = self.library.catalog.clone();
        let seeds = c.ids(&c.albums[ix].tracks);
        if let Some(&start) = seeds.first() {
            self.play_similar(seeds, start, cx);
        }
    }

    /// Play similar for an artist: their most-played track, then music like
    /// their songs.
    pub fn play_similar_artist(&mut self, artist_id: i64, cx: &mut Context<Self>) {
        let Some(ix) = self.artist_index(artist_id) else {
            return;
        };
        let c = self.library.catalog.clone();
        let list = &c.artists[ix].tracks;
        let seeds = c.ids(list);
        let start = list
            .iter()
            .max_by_key(|&&i| (c.track(i).play_count, std::cmp::Reverse(i)))
            .map(|&i| c.track(i).id);
        if let Some(start) = start {
            self.play_similar(seeds, start, cx);
        }
    }

    /// The player changed: remember what started, and let Radio top up the queue.
    pub fn reco_player_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.player.read(cx).current_id()
            && self.reco.recent.back() != Some(&id)
        {
            self.reco.recent.push_back(id);
            if self.reco.recent.len() > 10 {
                self.reco.recent.pop_front();
            }
        }
        self.maybe_extend_radio(cx);
    }

    /// Radio: when at most one track is left, append 10 like the last 3 played.
    pub fn maybe_extend_radio(&mut self, cx: &mut Context<Self>) {
        if !self.reco_on() || !self.settings.reco.radio {
            return;
        }
        let (current, left, queue, repeat) = {
            let p = self.player.read(cx);
            (p.current_id(), p.upcoming(2).len(), p.queue_ids(), p.repeat)
        };
        let Some(current) = current else { return };
        if repeat != lumen_core::RepeatMode::Off || left > 1 {
            return;
        }
        if self.reco.radio_done == Some((current, queue.len())) {
            return;
        }
        self.reco.radio_done = Some((current, queue.len()));
        let seeds: Vec<TrackId> = self
            .reco
            .recent
            .iter()
            .rev()
            .take(RADIO_SEEDS)
            .copied()
            .collect();
        let exclude: HashSet<TrackId> = queue.into_iter().collect();
        let ids = self.recommend(&seeds, RADIO_ADD, &exclude);
        if ids.is_empty() {
            return;
        }
        tracing::info!("radio: adding {} tracks", ids.len());
        let tracks = self.tracks_of(&ids);
        self.player.update(cx, |p, cx| p.enqueue(tracks, false, cx));
    }

    /// "MORE LIKE THIS" for the playing track (cached until it or the model changes).
    pub fn more_like_this(&mut self, cx: &mut Context<Self>) -> Vec<Track> {
        if !self.reco_on() {
            return Vec::new();
        }
        let (current, queue) = {
            let p = self.player.read(cx);
            (p.current_id(), p.queue_ids())
        };
        let Some(current) = current else {
            return Vec::new();
        };
        let fresh = matches!(&self.reco.more_like, Some((id, v, _)) if *id == current && *v == self.reco.version);
        if !fresh {
            let exclude: HashSet<TrackId> = queue.into_iter().collect();
            let ids = self.recommend(&[current], MORE_LIKE_THIS, &exclude);
            self.reco.more_like = Some((current, self.reco.version, ids));
        }
        let ids = self
            .reco
            .more_like
            .as_ref()
            .map(|m| m.2.clone())
            .unwrap_or_default();
        self.tracks_of(&ids)
    }

    /// Plays a More like this track now, keeping the queue.
    pub fn play_now(&mut self, id: TrackId, cx: &mut Context<Self>) {
        let tracks = self.tracks_of(&[id]);
        self.player.update(cx, |p, cx| {
            p.enqueue(tracks, true, cx);
            p.next();
        });
    }
}
