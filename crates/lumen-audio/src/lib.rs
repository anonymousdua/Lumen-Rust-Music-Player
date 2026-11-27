//! Lumen audio engine: decoding (symphonia), WASAPI exclusive/shared output,
//! gapless queue, ReplayGain, volume and seeking.
//!
//! Start an [`Engine`], send it [`Command`]s and read [`Event`]s:
//!
//! ```no_run
//! use lumen_audio::{Command, Engine, EngineConfig, QueueItem};
//! use std::time::Duration;
//!
//! let engine = Engine::start(EngineConfig::default()).expect("engine");
//! engine.send(Command::SetQueue {
//!     items: vec![QueueItem::from_path(r"C:\Music\song.flac")],
//!     start: 0,
//!     position: Duration::ZERO,
//!     play: true,
//! });
//! for event in engine.events().iter() {
//!     println!("{event:?}");
//! }
//! ```

pub mod decode;
pub mod dsp;
pub mod engine;
pub mod negotiate;
pub mod output;
mod priority;
pub mod protocol;
pub mod queue;
pub mod resample;

pub use engine::{Engine, EngineConfig};
pub use output::Backend;
pub use output::capture::{CaptureDevice, CapturedStream};
pub use protocol::*;

/// Lists the WASAPI render devices (for settings and `lumen-play`).
pub fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    Backend::Wasapi.list_devices()
}
