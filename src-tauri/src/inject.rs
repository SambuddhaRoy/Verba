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
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_LCONTROL, VK_LMENU,
    VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RETURN, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
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
    send(&events_for(text))
}

fn send(events: &[INPUT]) -> Result<()> {
    let sent = unsafe { SendInput(events, size_of::<INPUT>() as i32) };

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

/// Diagnostic for `--press`: hold keys down for `ms`, then release them.
/// Injected input, so it exercises the hotkey watchdog rather than the hook.
pub fn hold_keys(keys: &[VIRTUAL_KEY], ms: u64) -> Result<()> {
    send(&keys.iter().map(|&k| vkey(k, false)).collect::<Vec<_>>())?;
    std::thread::sleep(Duration::from_millis(ms));
    send(&keys.iter().rev().map(|&k| vkey(k, true)).collect::<Vec<_>>())
}

/// Left and right separately: an injected key-up for the generic VK_CONTROL
/// does not release a right Ctrl that is physically held.
const MODIFIERS: [VIRTUAL_KEY; 8] =
    [VK_LCONTROL, VK_RCONTROL, VK_LSHIFT, VK_RSHIFT, VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN];

/// Key-ups for whichever modifiers are down right now.
///
/// Live typing happens while the user is still holding the hotkey, and an app
/// that sees Ctrl+Shift down treats a typed "p" as Ctrl+Shift+P. Releasing
/// them in the target app first makes the text arrive as text. They are not
/// pressed again afterwards: the user lets go in a moment anyway, and
/// restoring a key that was physically released in between would leave it
/// stuck down.
fn lift_modifiers() -> Vec<INPUT> {
    let held: Vec<VIRTUAL_KEY> = MODIFIERS.iter().copied().filter(|&vk| is_down(vk)).collect();
    let mut out = Vec::new();
    // Alt or Win let go on their own are a gesture (menu bar, Start). A key
    // that does nothing, pressed in between, makes it a combination instead.
    if held.iter().any(|vk| matches!(*vk, VK_LMENU | VK_RMENU | VK_LWIN | VK_RWIN)) {
        out.push(vkey(VIRTUAL_KEY(0xE8), false));
        out.push(vkey(VIRTUAL_KEY(0xE8), true));
    }
    out.extend(held.into_iter().map(|vk| vkey(vk, true)));
    out
}

/// How to turn `typed` into `target`: backspaces to remove, then text to add.
/// Pure, so the arithmetic can be tested without a keyboard.
fn edit(typed: &str, target: &str) -> (usize, String) {
    let common = typed.chars().zip(target.chars()).take_while(|(a, b)| a == b).count();
    (typed.chars().count() - common, target.chars().skip(common).collect())
}

/// Text that is typed in pieces and corrected in place.
#[derive(Default)]
pub struct Typist {
    typed: String,
}

impl Typist {
    /// Make the document hold `target`, typing only what changed.
    pub fn set(&mut self, target: &str) -> Result<()> {
        let (back, add) = edit(&self.typed, target);
        if back == 0 && add.is_empty() {
            return Ok(());
        }
        let mut events = lift_modifiers();
        for _ in 0..back {
            events.push(vkey(VK_BACK, false));
            events.push(vkey(VK_BACK, true));
        }
        events.extend(events_for(&add));
        send(&events)?;
        self.typed = target.to_string();
        Ok(())
    }

    pub fn typed(&self) -> &str {
        &self.typed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extending_text_types_only_the_new_part() {
        assert_eq!(edit("Hello", "Hello world"), (0, " world".to_string()));
        assert_eq!(edit("", "Hi"), (0, "Hi".to_string()));
    }

    #[test]
    fn a_changed_word_is_backspaced_and_retyped() {
        // They share "the", so six characters come off and "re cat" goes on.
        assert_eq!(edit("their cat", "there cat"), (6, "re cat".to_string()));
    }

    #[test]
    fn identical_text_changes_nothing() {
        assert_eq!(edit("same", "same"), (0, String::new()));
    }

    #[test]
    fn shrinking_text_only_backspaces() {
        assert_eq!(edit("abc def", "abc"), (4, String::new()));
    }

    #[test]
    fn counts_characters_not_bytes() {
        // One backspace removes one character, however many bytes it takes.
        assert_eq!(edit("caf\u{e9}", "caf"), (1, String::new()));
        assert_eq!(edit("ok \u{1F600}", "ok"), (2, String::new()));
    }

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
