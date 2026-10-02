//! Minimal logging: console when there is one, always a file.
//!
//! As a GUI-subsystem binary there is no console when launched from Explorer,
//! so a log file is the only way to see what happened. Launched from a terminal
//! it attaches to the parent console and behaves like a normal CLI program.

use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("Verba").join("verba.log")
}

/// Attach to the launching terminal if there is one, and open the log file.
pub fn init() {
    unsafe {
        // ATTACH_PARENT_PROCESS. Fails harmlessly when launched from Explorer.
        let _ = windows::Win32::System::Console::AttachConsole(u32::MAX);
        let _ = windows::Win32::System::Console::SetConsoleOutputCP(65001);
    }

    let p = path();
    if let Some(dir) = p.parent() {
        let _ = create_dir_all(dir);
    }
    // Appended to, not truncated. Every diagnostic subcommand (--press, --state,
    // --format) runs this too, and truncating wiped the log of the instance
    // already running, leaving zero padding where its history had been. The
    // file restarts once it passes a megabyte so it cannot grow without end.
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::remove_file(&p);
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&p) {
        *FILE.lock().unwrap() = Some(f);
    }
}

/// Local wall-clock time. The file used to have none, so a report of "it
/// stopped working after a while" could not be lined up with anything.
fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}

pub fn write(line: &str) {
    println!("{line}");
    if let Ok(mut guard) = FILE.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{} {line}", stamp());
            let _ = f.flush();
        }
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
