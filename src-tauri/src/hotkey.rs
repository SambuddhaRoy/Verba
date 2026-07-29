//! Global hold-to-talk hotkey via a low-level keyboard hook.
//!
//! `RegisterHotKey` is unusable here: it fires on key-down only and never reports
//! release, so it cannot express hold-to-talk. `WH_KEYBOARD_LL` sees both edges.
//!
//! The hook procedure runs on the hook thread and must return fast — Windows
//! silently unhooks a callback that exceeds `LowLevelHooksTimeout` (~300ms default).
//! So it does nothing but flip an atomic and push to a channel.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_SHIFT, VK_SPACE,
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

fn key_down(vk: i32) -> bool {
    // GetAsyncKeyState sets the high bit while the key is physically down.
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };

        // Ignore anything we synthesized ourselves (inject.rs sends Ctrl+V), or we
        // would react to our own paste.
        let injected = kb.flags.0 & LLKHF_INJECTED.0 != 0;

        if !injected && kb.vkCode == VK_SPACE.0 as u32 {
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

            if down && key_down(VK_CONTROL.0 as i32) && key_down(VK_SHIFT.0 as i32) {
                if !HELD.swap(true, Ordering::SeqCst) {
                    send(Event::Pressed);
                }
                return LRESULT(1); // swallow: the focused app must not see it
            }

            // Release is matched on Space alone — the user may lift Ctrl or Shift
            // first, and we still owe a Released for the Pressed we sent.
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
