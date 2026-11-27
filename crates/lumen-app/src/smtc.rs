//! Windows System Media Transport Controls: media keys, the volume-flyout
//! media overlay and the lock screen show the track and control playback.
//!
//! Talks to WinRT through the `windows` crate (the same version the audio
//! engine uses), via `ISystemMediaTransportControlsInterop::GetForWindow`.
//! Updates run on a small worker thread because loading the cover thumbnail
//! is a blocking WinRT file call.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use lumen_audio::PlaybackState;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::StorageFile;
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::core::HSTRING;

/// A button pressed on the keyboard or in the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaButton {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    /// The overlay's seek bar (where supported).
    Seek(Duration),
}

/// What the overlay shows.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub thumbnail: Option<PathBuf>,
}

enum Update {
    Info(Option<MediaInfo>),
    State(MediaPlaybackStatus, bool),
    Timeline(Duration, Duration),
}

pub struct Smtc {
    tx: mpsc::Sender<Update>,
}

impl Smtc {
    /// Registers with the window's media session. `on_button` runs on a
    /// WinRT thread.
    pub fn new(
        hwnd: isize,
        on_button: impl Fn(MediaButton) + Send + Sync + Clone + 'static,
    ) -> windows::core::Result<Self> {
        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;
        // SAFETY: `hwnd` is the live main window, owned by this process.
        #[allow(unsafe_code)]
        let controls: SystemMediaTransportControls =
            unsafe { interop.GetForWindow(HWND(hwnd as *mut core::ffi::c_void))? };
        controls.SetIsEnabled(true)?;
        controls.SetIsPlayEnabled(true)?;
        controls.SetIsPauseEnabled(true)?;
        controls.SetIsStopEnabled(true)?;
        controls.SetIsNextEnabled(true)?;
        controls.SetIsPreviousEnabled(true)?;
        controls.SetPlaybackStatus(MediaPlaybackStatus::Closed)?;

        let buttons = on_button.clone();
        controls.ButtonPressed(&TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                let b = match args.Button()? {
                    SystemMediaTransportControlsButton::Play => Some(MediaButton::Play),
                    SystemMediaTransportControlsButton::Pause => Some(MediaButton::Pause),
                    SystemMediaTransportControlsButton::Stop => Some(MediaButton::Stop),
                    SystemMediaTransportControlsButton::Next => Some(MediaButton::Next),
                    SystemMediaTransportControlsButton::Previous => Some(MediaButton::Previous),
                    _ => None,
                };
                if let Some(b) = b {
                    buttons(b);
                }
            }
            Ok(())
        }))?;
        let seeks = on_button;
        controls.PlaybackPositionChangeRequested(&TypedEventHandler::<
            SystemMediaTransportControls,
            PlaybackPositionChangeRequestedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                let t = args.RequestedPlaybackPosition()?;
                seeks(MediaButton::Seek(Duration::from_nanos(
                    (t.Duration.max(0) as u64) * 100,
                )));
            }
            Ok(())
        }))?;

        let (tx, rx) = mpsc::channel::<Update>();
        std::thread::Builder::new()
            .name("lumen-smtc".into())
            .spawn(move || {
                for u in rx {
                    // Only the latest state matters after a burst.
                    if let Err(e) = apply(&controls, u) {
                        tracing::debug!("media controls update failed: {e}");
                    }
                }
            })
            .map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e.to_string()))?;
        Ok(Self { tx })
    }

    pub fn set_info(&self, info: Option<MediaInfo>) {
        let _ = self.tx.send(Update::Info(info));
    }

    pub fn set_state(&self, state: PlaybackState, has_track: bool) {
        let status = match (state, has_track) {
            (_, false) => MediaPlaybackStatus::Closed,
            (PlaybackState::Playing, _) => MediaPlaybackStatus::Playing,
            (PlaybackState::Paused, _) => MediaPlaybackStatus::Paused,
            (PlaybackState::Stopped, _) => MediaPlaybackStatus::Stopped,
        };
        let _ = self.tx.send(Update::State(status, has_track));
    }

    pub fn set_timeline(&self, position: Duration, total: Duration) {
        let _ = self.tx.send(Update::Timeline(position, total));
    }
}

fn span(d: Duration) -> TimeSpan {
    TimeSpan {
        Duration: (d.as_nanos() / 100) as i64,
    }
}

fn apply(c: &SystemMediaTransportControls, u: Update) -> windows::core::Result<()> {
    match u {
        Update::Info(None) => {
            let d = c.DisplayUpdater()?;
            d.ClearAll()?;
            d.Update()?;
        }
        Update::Info(Some(info)) => {
            let d = c.DisplayUpdater()?;
            d.SetType(MediaPlaybackType::Music)?;
            let m = d.MusicProperties()?;
            m.SetTitle(&HSTRING::from(info.title.as_str()))?;
            m.SetArtist(&HSTRING::from(info.artist.as_str()))?;
            m.SetAlbumTitle(&HSTRING::from(info.album.as_str()))?;
            let thumb = info.thumbnail.and_then(|p| {
                let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(p.as_os_str()))
                    .ok()?
                    .join()
                    .ok()?;
                RandomAccessStreamReference::CreateFromFile(&file).ok()
            });
            match thumb {
                Some(t) => d.SetThumbnail(&t)?,
                None => d.SetThumbnail(None)?,
            }
            d.Update()?;
        }
        Update::State(status, has_track) => {
            c.SetPlaybackStatus(status)?;
            c.SetIsNextEnabled(has_track)?;
            c.SetIsPreviousEnabled(has_track)?;
        }
        Update::Timeline(position, total) => {
            let t = SystemMediaTransportControlsTimelineProperties::new()?;
            t.SetStartTime(span(Duration::ZERO))?;
            t.SetMinSeekTime(span(Duration::ZERO))?;
            t.SetEndTime(span(total))?;
            t.SetMaxSeekTime(span(total))?;
            t.SetPosition(span(position.min(total)))?;
            c.UpdateTimelineProperties(&t)?;
        }
    }
    Ok(())
}
