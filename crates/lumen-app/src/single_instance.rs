//! One Lumen per library: launching it again focuses the running window.
//!
//! A named mutex marks the running instance. Its name includes a hash of the
//! data folder, so a test run with `LUMEN_DATA_DIR` set doesn't collide with
//! your everyday Lumen.

#![allow(unsafe_code)]

use std::path::Path;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, MAX_PATH,
};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
    SetForegroundWindow, ShowWindow,
};
use windows::core::{BOOL, HSTRING, PWSTR};

/// Window title of the main window.
const TITLE: &str = "Lumen";

/// True if this is the only instance for `data_dir`. Otherwise brings the
/// running instance's window to the front and returns false.
pub fn acquire(data_dir: &Path) -> bool {
    let key = data_dir.to_string_lossy().to_lowercase();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let name = HSTRING::from(format!("Local\\Lumen-{hash:016x}"));
    // SAFETY: plain Win32 calls; the handle is intentionally kept open for
    // the life of the process (that is what marks the instance).
    let first = unsafe {
        match CreateMutexW(None, true, &name) {
            Ok(_handle) => GetLastError() != ERROR_ALREADY_EXISTS,
            // Can't tell: don't block startup.
            Err(_) => true,
        }
    };
    if !first {
        focus_existing();
    }
    first
}

/// Finds a visible "Lumen" window owned by a process running the same
/// executable name, restores it and brings it to the front.
fn focus_existing() {
    let own = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .unwrap_or_else(|| "lumen.exe".into());
    let mut found = Found {
        exe: own,
        hwnd: None,
    };
    // SAFETY: the callback only reads window properties; `found` outlives
    // the synchronous EnumWindows call.
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut found as *mut Found as isize));
        if let Some(h) = found.hwnd {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
    }
}

struct Found {
    exe: String,
    hwnd: Option<HWND>,
}

unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the `Found` passed by `focus_existing`.
    let found = unsafe { &mut *(lparam.0 as *mut Found) };
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        let mut title = [0u16; 64];
        let n = GetWindowTextW(hwnd, &mut title);
        if String::from_utf16_lossy(&title[..n.max(0) as usize]) != TITLE {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() || exe_name(pid).as_deref() != Some(found.exe.as_str()) {
            return true.into();
        }
    }
    found.hwnd = Some(hwnd);
    false.into()
}

/// Lowercased executable file name of process `pid`.
fn exe_name(pid: u32) -> Option<String> {
    // SAFETY: query-only access; the handle is closed before returning.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(process);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
    }
}
