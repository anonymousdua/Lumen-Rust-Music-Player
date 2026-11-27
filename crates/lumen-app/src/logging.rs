//! Log files in `%LOCALAPPDATA%\Lumen\logs` (or `LUMEN_DATA_DIR\logs`):
//!
//! - `lumen.log`: warnings and errors from every crate (skipped files, device
//!   problems, audio underruns), kept to about 4 MB with one older copy.
//! - `panics.log`: every panic on any thread, with its thread name, location
//!   and backtrace. Background threads also catch their panics, so the app
//!   keeps running; this records what happened.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Sets up logging and the panic hook. Returns the logs folder.
pub fn init(data_dir: &Path) -> Option<PathBuf> {
    let dir = data_dir.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    let log = dir.join("lumen.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&log, dir.join("lumen.1.log"));
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&log) {
        let level = if std::env::var_os("LUMEN_LOG_INFO").is_some() {
            tracing::Level::INFO
        } else {
            tracing::Level::WARN
        };
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(level)
            .with_thread_names(true)
            .with_writer(Mutex::new(file))
            .try_init();
    }
    install_panic_hook(dir.join("panics.log"));
    tracing::info!("Lumen {} started", env!("CARGO_PKG_VERSION"));
    Some(dir)
}

fn install_panic_hook(path: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed").to_string();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let when = crate::ui::now_ms();
        let entry = format!(
            "=== panic at {} (unix ms {when}) on thread '{name}'\n{message}\n  at {location}\n{backtrace}\n",
            crate::ui::format_date(when),
        );
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(entry.as_bytes());
        }
        tracing::error!("panic on thread '{name}': {message} at {location}");
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panics_on_threads_are_written_to_the_log() {
        let dir = std::env::temp_dir().join(format!("lumen-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logs = init(&dir).expect("logs dir");
        let handle = std::thread::Builder::new()
            .name("lumen-test-worker".into())
            .spawn(|| {
                let r = std::panic::catch_unwind(|| panic!("boom in a worker"));
                assert!(r.is_err());
            })
            .expect("spawn");
        handle.join().expect("join");
        let text = std::fs::read_to_string(logs.join("panics.log")).expect("panics.log");
        assert!(text.contains("boom in a worker"), "{text}");
        assert!(text.contains("lumen-test-worker"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
