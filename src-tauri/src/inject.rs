//! Text insertion at the caret of whatever window has focus.
//!
//! Synthesizes the characters directly with `KEYEVENTF_UNICODE` rather than
//! staging on the clipboard and sending Ctrl+V. The clipboard route has three
//! independent failure modes — the staged text has to land, the target has to
//! consume it before we restore, and the app has to honour Ctrl+V at all — and
//! it destroys whatever the user had copied. Unicode events have none of that:
//! the whole transcript goes out in one `SendInput` call, and the app receives
//! ordinary WM_CHAR messages it cannot distinguish from typing.

use anyhow::{anyhow, Result};
use std::mem::size_of;
use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RETURN,
    VK_RWIN, VK_SHIFT,
};

fn is_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

/// Block until the user has physically let go of every modifier.
///
/// Hold-to-talk means Ctrl and Shift are still down at the moment Space is
/// released. Characters injected while Ctrl is held arrive as control codes
/// rather than text, so the first few characters of every dictation would be
/// eaten if we didn't wait.
fn wait_for_modifiers_released() {
    let deadline = Instant::now() + Duration::from_secs(2);
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
    eprintln!("  warning: modifiers still held after 2s, injecting anyway");
}

/// A virtual-key event, for keys that have no character (Enter).
fn vkey(vk: VIRTUAL_KEY, up: bool) -> INPUT {
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

/// A single UTF-16 code unit delivered as a character, bypassing the keyboard
/// layout entirely. `wVk` must be zero; the unit rides in `wScan`.
fn unicode(unit: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: if up {
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                } else {
                    KEYEVENTF_UNICODE
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn events_for(text: &str) -> Vec<INPUT> {
    let mut out = Vec::with_capacity(text.len() * 2 + 8);
    for ch in text.chars() {
        match ch {
            // Unicode events deliver \n as a literal control character, which
            // most editors ignore. Newlines have to be a real Enter keypress.
            '\n' => {
                out.push(vkey(VK_RETURN, false));
                out.push(vkey(VK_RETURN, true));
            }
            '\r' => {}
            _ => {
                // encode_utf16 splits astral characters into a surrogate pair;
                // sending each unit separately is exactly what Windows expects.
                let mut buf = [0u16; 2];
                for &mut unit in ch.encode_utf16(&mut buf) {
                    out.push(unicode(unit, false));
                    out.push(unicode(unit, true));
                }
            }
        }
    }
    out
}

/// Type `text` at the caret. Leaves the clipboard untouched.
pub fn insert(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    wait_for_modifiers_released();

    let events = events_for(text);
    let sent = unsafe { SendInput(&events, size_of::<INPUT>() as i32) };

    // A partial or zero return means the input was blocked — most often UIPI,
    // when the focused window belongs to an elevated process and we are not.
    // Silently swallowing this is what made the first version look like it did
    // nothing at all.
    if sent as usize != events.len() {
        let err = windows::core::Error::from_thread();
        return Err(anyhow!(
            "injected {sent}/{} events: {err}. If the target app runs elevated, \
             Verba must too.",
            events.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Event construction is pure, so it can be checked without touching the
    /// desktop — calling `insert` in a test would type into whatever window
    /// happened to have focus while the suite ran.
    #[test]
    fn every_character_becomes_a_down_up_pair() {
        assert_eq!(events_for("abc").len(), 6);
        assert_eq!(events_for("").len(), 0);
    }

    #[test]
    fn newline_becomes_enter_not_a_control_character() {
        let ev = events_for("\n");
        assert_eq!(ev.len(), 2);
        unsafe {
            assert_eq!(ev[0].Anonymous.ki.wVk, VK_RETURN);
            // A real key, not a unicode payload.
            assert_eq!(ev[0].Anonymous.ki.wScan, 0);
        }
    }

    #[test]
    fn astral_characters_split_into_surrogate_pairs() {
        // One emoji is two UTF-16 units, so four events.
        assert_eq!(events_for("\u{1F600}").len(), 4);
        // Accented Latin stays in the BMP: one unit, two events.
        assert_eq!(events_for("ü").len(), 2);
    }

    #[test]
    fn carriage_returns_are_dropped() {
        assert_eq!(events_for("\r\n").len(), events_for("\n").len());
    }
}
