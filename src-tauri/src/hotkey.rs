//! Global hold-to-talk hotkey via a low-level keyboard hook.
//!
//! `RegisterHotKey` is unusable here: it fires on key-down only and never
//! reports release, so it cannot express hold-to-talk. `WH_KEYBOARD_LL` sees
//! both edges.
//!
//! The hook procedure runs on the hook thread and must return fast — Windows
//! silently unhooks a callback that exceeds `LowLevelHooksTimeout` (~300ms
//! default). So it does nothing but read two atomics and push to a channel.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Pressed,
    Released,
}

static TX: OnceLock<Sender<Event>> = OnceLock::new();
/// Whether *our* combo is currently held. Guards against key auto-repeat, which
/// fires WM_KEYDOWN continuously while a key is held.
static HELD: AtomicBool = AtomicBool::new(false);

/// The live binding. Atomics rather than a OnceLock so changing the hotkey in
/// settings takes effect immediately instead of needing a restart.
static VK: AtomicU32 = AtomicU32::new(0x20); // Space
static MODS: AtomicU32 = AtomicU32::new(0b0011); // Ctrl | Shift

pub fn set_binding(vk: u32, mods: u32) {
    VK.store(vk, Ordering::Relaxed);
    MODS.store(mods, Ordering::Relaxed);
    // A rebind while held would otherwise leave us owing a Released that can
    // never match.
    HELD.store(false, Ordering::Relaxed);
}

fn key_down(vk: i32) -> bool {
    // GetAsyncKeyState sets the high bit while the key is physically down.
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

/// Do the physically-held modifiers match the binding exactly? Requiring an
/// exact match means Ctrl+Shift+Alt+Space does not fire a Ctrl+Shift+Space
/// binding, so bindings that differ only by an extra modifier stay distinct.
fn mods_match(want: u32) -> bool {
    let ctrl = key_down(VK_CONTROL.0 as i32);
    let shift = key_down(VK_SHIFT.0 as i32);
    let alt = key_down(VK_MENU.0 as i32);
    let win = key_down(VK_LWIN.0 as i32) || key_down(VK_RWIN.0 as i32);
    let have = (ctrl as u32) | (shift as u32) << 1 | (alt as u32) << 2 | (win as u32) << 3;
    have == want
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };

        // Ignore anything we synthesized ourselves (inject.rs sends characters),
        // or we would react to our own output.
        let injected = kb.flags.0 & LLKHF_INJECTED.0 != 0;

        if !injected && kb.vkCode == VK.load(Ordering::Relaxed) {
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

            if down && mods_match(MODS.load(Ordering::Relaxed)) {
                if !HELD.swap(true, Ordering::SeqCst) {
                    send(Event::Pressed);
                }
                return LRESULT(1); // swallow: the focused app must not see it
            }

            // Release is matched on the main key alone — the user may lift a
            // modifier first, and we still owe a Released for the Pressed we
            // sent.
            if up && HELD.swap(false, Ordering::SeqCst) {
                send(Event::Released);
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn send(ev: Event) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(ev);
    }
}

/// Installs the hook on a dedicated thread and returns the event stream.
///
/// The hook must be installed from a thread that pumps messages, so that thread
/// parks in `GetMessageW` forever; it owns the hook for the life of the process.
pub fn spawn() -> Result<Receiver<Event>> {
    let (tx, rx) = channel();
    TX.set(tx).map_err(|_| anyhow!("hotkey already started"))?;

    let (ready_tx, ready_rx) = channel();

    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || unsafe {
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0);
            let _ = ready_tx.send(hook.as_ref().err().map(|e| e.to_string()));
            if hook.is_err() {
                return;
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = DispatchMessageW(&msg);
            }
        })?;

    match ready_rx.recv()? {
        None => Ok(rx),
        Some(e) => Err(anyhow!("SetWindowsHookExW failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    /// The packing the hook reads has to agree with what Config produces, or a
    /// rebind silently binds the wrong modifiers.
    #[test]
    fn modifier_packing_matches_config() {
        let hk = crate::config::Hotkey {
            ctrl: true, shift: false, alt: true, win: false,
            vk: 0x41, label: "A".into(),
        };
        assert_eq!(hk.mods(), 0b0101);
        assert_eq!(crate::config::Hotkey::default().mods(), 0b0011);
    }
}
