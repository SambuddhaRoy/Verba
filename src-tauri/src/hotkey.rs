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
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_APP,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
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

/// Thread message asking the hook thread to replace its hook.
const WM_REHOOK: u32 = WM_APP + 1;
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
/// Set while a press that the watchdog started, not the hook, is outstanding.
static WATCHDOG_HELD: AtomicBool = AtomicBool::new(false);

const WATCH_EVERY: Duration = Duration::from_millis(40);
/// Polls in a row that must disagree with the hook before the watchdog acts.
/// The hook normally answers within microseconds, so three is about 120ms of
/// proof rather than a race.
const WATCH_CONFIRM: u32 = 3;

#[derive(Debug, PartialEq)]
enum Recovery {
    Press,
    Release,
}

/// The decision, kept pure so it can be tested without a keyboard.
///
/// Windows removes a low-level hook without telling anyone if its callback is
/// ever slow, and a process that has sat idle for hours has had its pages
/// trimmed, so the first keystroke after a long quiet spell is the likeliest
/// to be slow. Afterwards the hotkey is dead until restart. Two cases:
///
/// - the key is physically down with the right modifiers and the hook never
///   reported it: the hook is gone, start the dictation ourselves.
/// - we started one, and the key has come up: end it. Only for presses the
///   watchdog started. A hook that is alive swallows the key, and a swallowed
///   key may never reach the OS key table, so "up" there proves nothing.
fn watch_step(
    missed: &mut u32,
    gone: &mut u32,
    key_down: bool,
    mods_ok: bool,
    held: bool,
    ours: bool,
) -> Option<Recovery> {
    *missed = if key_down && mods_ok && !held { *missed + 1 } else { 0 };
    *gone = if ours && held && !key_down { *gone + 1 } else { 0 };
    if *missed >= WATCH_CONFIRM {
        *missed = 0;
        return Some(Recovery::Press);
    }
    if *gone >= WATCH_CONFIRM {
        *gone = 0;
        return Some(Recovery::Release);
    }
    None
}

fn watchdog() {
    let (mut missed, mut gone) = (0, 0);
    loop {
        std::thread::sleep(WATCH_EVERY);
        let held = HELD.load(Ordering::SeqCst);
        if !held {
            WATCHDOG_HELD.store(false, Ordering::SeqCst);
        }
        let ours = WATCHDOG_HELD.load(Ordering::SeqCst);
        let step = watch_step(
            &mut missed,
            &mut gone,
            key_down(VK.load(Ordering::Relaxed) as i32),
            mods_match(MODS.load(Ordering::Relaxed)),
            held,
            ours,
        );
        match step {
            Some(Recovery::Press) => {
                crate::log!("hotkey: the keyboard hook missed a press, recovering");
                HELD.store(true, Ordering::SeqCst);
                WATCHDOG_HELD.store(true, Ordering::SeqCst);
                send(Event::Pressed);
                // Put the hook back so the next press is swallowed again.
                unsafe {
                    let _ = PostThreadMessageW(
                        HOOK_THREAD.load(Ordering::Relaxed),
                        WM_REHOOK,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            Some(Recovery::Release) => {
                crate::log!("hotkey: the release came up without the hook, ending the dictation");
                HELD.store(false, Ordering::SeqCst);
                WATCHDOG_HELD.store(false, Ordering::SeqCst);
                send(Event::Released);
            }
            None => {}
        }
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
            HOOK_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
            let mut hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0);
            let _ = ready_tx.send(hook.as_ref().err().map(|e| e.to_string()));
            if hook.is_err() {
                return;
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_REHOOK {
                    if let Ok(old) = hook {
                        let _ = UnhookWindowsHookEx(old);
                    }
                    hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0);
                    crate::log!(
                        "hotkey: keyboard hook reinstalled ({})",
                        if hook.is_ok() { "ok" } else { "failed" }
                    );
                    continue;
                }
                let _ = DispatchMessageW(&msg);
            }
        })?;
    std::thread::Builder::new().name("hotkey-watch".into()).spawn(watchdog)?;

    match ready_rx.recv()? {
        None => Ok(rx),
        Some(e) => Err(anyhow!("SetWindowsHookExW failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Step = (bool, bool, bool, bool);

    fn run(steps: &[Step]) -> Vec<Option<Recovery>> {
        let (mut missed, mut gone) = (0, 0);
        steps
            .iter()
            .map(|&(key, mods, held, ours)| watch_step(&mut missed, &mut gone, key, mods, held, ours))
            .collect()
    }

    #[test]
    fn a_press_the_hook_reported_is_left_alone() {
        // Hook alive: HELD is already true by the time the key reads as down.
        assert!(run(&[(true, true, true, false); 10]).iter().all(|o| o.is_none()));
    }

    #[test]
    fn a_press_the_hook_missed_is_recovered_after_three_polls() {
        let out = run(&[(true, true, false, false); 4]);
        assert_eq!(out, vec![None, None, Some(Recovery::Press), None]);
    }

    #[test]
    fn the_wrong_modifiers_never_recover_anything() {
        // Ctrl+Shift+Alt+Space must not fire a Ctrl+Shift+Space binding.
        assert!(run(&[(true, false, false, false); 10]).iter().all(|o| o.is_none()));
    }

    #[test]
    fn a_blip_shorter_than_the_debounce_is_ignored() {
        let down: Step = (true, true, false, false);
        let up: Step = (false, true, false, false);
        assert!(run(&[down, down, up, down]).iter().all(|o| o.is_none()));
    }

    #[test]
    fn release_is_only_taken_over_for_a_press_the_watchdog_started() {
        // Key reads up while HELD: with a live hook that is just a swallowed key.
        assert!(run(&[(false, true, true, false); 10]).iter().all(|o| o.is_none()));
        let out = run(&[(false, true, true, true); 3]);
        assert_eq!(out, vec![None, None, Some(Recovery::Release)]);
    }

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
