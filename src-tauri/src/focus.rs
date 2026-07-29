//! Which app currently has focus — the input to mode routing in M3.

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};

#[derive(Debug, Clone, Default)]
pub struct App {
    /// Bare executable name, e.g. "Code.exe".
    pub exe: String,
    pub title: String,
}

fn process_exe(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(h);

        if !ok {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(full.rsplit('\\').next().unwrap_or(&full).to_string())
    }
}

/// Snapshot the foreground window.
///
/// Taken at key-down, not at insert time: it's the same window either way since
/// the overlay never takes focus, but capturing early means the routing decision
/// reflects where the user was actually looking when they started talking.
pub fn foreground() -> App {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return App::default();
        }

        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        App {
            exe: process_exe(pid).unwrap_or_default(),
            title,
        }
    }
}
