//! Overlay window behaviour: never take focus, never intercept clicks.
//!
//! Tauri's `focus: false` only affects the initial show. The guarantee we need
//! is structural and comes from the extended window styles.

use anyhow::Result;
use tauri::{PhysicalPosition, WebviewWindow};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT,
};

/// Apply the styles that make this a true overlay, and park it top-centre.
///
/// - `WS_EX_NOACTIVATE`  never becomes the foreground window, so the caret stays
///                       in the app behind us. Without this, injection has
///                       nowhere to land.
/// - `WS_EX_TRANSPARENT` clicks pass through to whatever is underneath.
/// - `WS_EX_TOOLWINDOW`  keeps it out of the taskbar and Alt+Tab.
pub fn configure(win: &WebviewWindow) -> Result<()> {
    let hwnd = HWND(win.hwnd()?.0 as _);
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let wanted = current
            | WS_EX_NOACTIVATE.0 as isize
            | WS_EX_TRANSPARENT.0 as isize
            | WS_EX_TOOLWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
    }
    disown_backdrop(hwnd);
    center_top(win)?;
    Ok(())
}

/// Tell DWM this window paints its own background and wants nothing added.
///
/// The overlay is a 760x420 transparent canvas with a much smaller box drawn
/// inside it, so anything that gives the *window* a backdrop fills that whole
/// rectangle — the user sees a large translucent panel floating around the
/// transcript. That is what happens under Windhawk's translucent-windows mod,
/// which applies acrylic to windows it matches.
///
/// Declaring DWMSBT_NONE and disabling non-client rendering is the documented
/// way to say "do not decorate me", and it is correct regardless of who is
/// asking. It is not a guarantee: a mod that sets the backdrop *after* us, or
/// that calls the undocumented SetWindowCompositionAttribute directly, will
/// still win — nothing inside this process can stop that. Excluding Verba in
/// the mod's own window-match rules is the reliable fix on the user's side.
fn disown_backdrop(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMNCRP_DISABLED, DWMSBT_NONE, DWMWA_NCRENDERING_POLICY,
        DWMWA_SYSTEMBACKDROP_TYPE,
    };

    unsafe {
        let backdrop = DWMSBT_NONE;
        // Unsupported before Windows 11 22H2, where it returns E_INVALIDARG.
        // Logged rather than surfaced: there is nothing the user could do, but
        // when someone reports a stray panel around the overlay the first
        // question is whether this even applied.
        match DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&backdrop) as u32,
        ) {
            Ok(()) => crate::log!("overlay: system backdrop disabled"),
            Err(e) => crate::log!("overlay: backdrop opt-out unavailable ({e})"),
        }

        let policy = DWMNCRP_DISABLED;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &policy as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&policy) as u32,
        );
    }
}

/// Top edge of the primary monitor, horizontally centred.
fn center_top(win: &WebviewWindow) -> Result<()> {
    let Some(monitor) = win.primary_monitor()? else {
        return Ok(());
    };
    // Physical pixels throughout — mixing these with logical units puts the
    // overlay off-centre on any scaled display, which is most laptops.
    let screen = monitor.size();
    let window = win.outer_size()?;
    let x = (screen.width as i32 - window.width as i32) / 2;
    win.set_position(PhysicalPosition::new(x.max(0), 0))?;
    Ok(())
}
