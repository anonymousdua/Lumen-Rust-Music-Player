//! Thread priorities: MMCSS "Pro Audio" for the device thread, above-normal
//! for the decoder thread.

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        AVRT_PRIORITY_HIGH, AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
        AvSetMmThreadPriority, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    };
    use windows::core::w;

    /// Registers the current thread with MMCSS as "Pro Audio" at high priority
    /// until dropped. Must be created and dropped on the same thread.
    pub struct ProAudioGuard(HANDLE);

    impl ProAudioGuard {
        pub fn join() -> Option<Self> {
            let mut task_index = 0u32;
            // SAFETY: the task name is a valid static wide string and task_index
            // outlives the call.
            let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
            match handle {
                Ok(h) => {
                    // SAFETY: `h` is the MMCSS handle just returned for this thread.
                    if let Err(e) = unsafe { AvSetMmThreadPriority(h, AVRT_PRIORITY_HIGH) } {
                        tracing::warn!("MMCSS priority not raised: {e}");
                    }
                    Some(Self(h))
                }
                Err(e) => {
                    tracing::warn!("MMCSS Pro Audio registration failed: {e}");
                    None
                }
            }
        }
    }

    impl Drop for ProAudioGuard {
        fn drop(&mut self) {
            // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread.
            let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
        }
    }

    pub fn raise_current_thread() {
        // SAFETY: GetCurrentThread returns a pseudo-handle valid for this thread.
        if let Err(e) =
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) }
        {
            tracing::warn!("could not raise decoder thread priority: {e}");
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct ProAudioGuard;
    impl ProAudioGuard {
        pub fn join() -> Option<Self> {
            None
        }
    }
    pub fn raise_current_thread() {}
}

pub use imp::{ProAudioGuard, raise_current_thread};
