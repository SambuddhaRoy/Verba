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
    center_top(win)?;
    Ok(())
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
