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
    // Truncated per run: this is a debugging aid, not an audit trail, and an
    // ever-growing file is its own problem.
    if let Ok(f) = OpenOptions::new().create(true).write(true).truncate(true).open(&p) {
        *FILE.lock().unwrap() = Some(f);
    }
}

pub fn write(line: &str) {
    println!("{line}");
    if let Ok(mut guard) = FILE.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
