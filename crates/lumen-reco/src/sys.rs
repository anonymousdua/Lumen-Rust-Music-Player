//! Windows specifics: background thread priority and local time.

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    /// Background mode: the lowest CPU, I/O and memory priority, so analysis
    /// only uses time nothing else wants (above all, the audio threads).
    pub fn background_thread() {
        // SAFETY: GetCurrentThread returns a pseudo-handle valid for this thread.
        if let Err(e) =
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) }
        {
            tracing::warn!("could not lower the analysis thread's priority: {e}");
        }
    }

    pub fn local_hour(ms: i64) -> Option<u8> {
        // FILETIME counts 100 ns intervals since 1601-01-01.
        let ticks = (ms.max(0) as u64 + 11_644_473_600_000) * 10_000;
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        // SAFETY: both pointers refer to live locals of the right type.
        let ok = unsafe {
            FileTimeToSystemTime(&ft, &mut utc).is_ok()
                && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
        };
        ok.then_some(local.wHour as u8)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn background_thread() {}

    pub fn local_hour(ms: i64) -> Option<u8> {
        Some(((ms.rem_euclid(86_400_000)) / 3_600_000) as u8)
    }
}

pub use imp::background_thread;

/// The local hour (0–23) at `ms` since the epoch (UTC if unknown).
pub fn local_hour(ms: i64) -> u8 {
    imp::local_hour(ms).unwrap_or(((ms.rem_euclid(86_400_000)) / 3_600_000) as u8)
}
