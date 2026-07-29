//! Text insertion at the caret of whatever window has focus.
//!
//! Clipboard + Ctrl+V rather than per-character `SendInput`: synthesizing unicode
//! keystrokes is reliable but slow, and plenty of apps (terminals, Electron) drop
//! or reorder a long burst. Paste is one event regardless of length.

use anyhow::Result;
use arboard::Clipboard;
use std::mem::size_of;
use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};

/// How long to let the target app consume the clipboard before we put the user's
/// own content back.
// ponytail: fixed delay, not a clipboard-viewer callback. Restoring too early
// would paste nothing; if a slow app ever loses the race, listen for
// WM_CLIPBOARDUPDATE instead of raising this number.
const PASTE_SETTLE: Duration = Duration::from_millis(150);

fn is_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

/// Block until the user has physically let go of every modifier.
///
/// Hold-to-talk means Ctrl and Shift are still down at the moment Space is
/// released. Sending Ctrl+V into that state produces Ctrl+Shift+V, which is
/// "paste without formatting" in some apps and nothing at all in others.
fn wait_for_modifiers_released() {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if !is_down(VK_CONTROL)
            && !is_down(VK_SHIFT)
            && !is_down(VK_MENU)
            && !is_down(VK_LWIN)
            && !is_down(VK_RWIN)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_ctrl_v() {
    let seq = [
        key(VK_CONTROL, false),
        key(VK_V, false),
        key(VK_V, true),
        key(VK_CONTROL, true),
    ];
    unsafe { SendInput(&seq, size_of::<INPUT>() as i32) };
}

/// Stage `text` on the clipboard, run `paste`, then put the user's content back.
///
/// `paste` is a parameter so the restore logic can be tested without synthesizing
/// keystrokes into whatever window happens to have focus.
fn with_clipboard(text: &str, paste: impl FnOnce()) -> Result<()> {
    // Fresh Clipboard per operation: arboard holds the Win32 clipboard open for
    // the lifetime of the handle, which blocks the app we're about to paste into.
    let saved = Clipboard::new().ok().and_then(|mut c| c.get_text().ok());

    Clipboard::new()?.set_text(text)?;
    paste();
    std::thread::sleep(PASTE_SETTLE);

    if let Some(prev) = saved {
        Clipboard::new()?.set_text(prev)?;
    }
    Ok(())
}

/// Paste `text` at the caret, leaving the clipboard as we found it.
pub fn insert(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    wait_for_modifiers_released();
    with_clipboard(text, send_ctrl_v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clipboard must survive a dictation untouched — losing what the user
    /// had copied is the kind of bug that makes people uninstall.
    ///
    /// Deliberately does not go through `insert`: that would SendInput a real
    /// Ctrl+V into whichever window has focus while the suite runs.
    #[test]
    fn clipboard_round_trips() {
        let Ok(mut cb) = Clipboard::new() else {
            eprintln!("no clipboard available, skipping");
            return;
        };
        let sentinel = "verba-test-sentinel-\u{1F600}-ünïcode";
        if cb.set_text(sentinel).is_err() {
            eprintln!("clipboard not writable, skipping");
            return;
        }
        drop(cb);

        let mut staged = None;
        with_clipboard("pasted text", || {
            staged = Clipboard::new().unwrap().get_text().ok();
        })
        .expect("with_clipboard failed");

        assert_eq!(
            staged.as_deref(),
            Some("pasted text"),
            "text was not on the clipboard when paste fired"
        );
        let after = Clipboard::new().unwrap().get_text().unwrap();
        assert_eq!(after, sentinel, "clipboard was not restored");
    }
}
