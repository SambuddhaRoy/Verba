//! Overlay window behaviour: never take focus, never intercept clicks.
//!
//! Tauri's `focus: false` only affects the initial show. The guarantee we need
//! is structural and comes from the extended window styles.

use anyhow::Result;
use tauri::{LogicalSize, PhysicalPosition, WebviewWindow};

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

/// The window is a canvas much larger than the panel drawn inside it, which is
/// what gives the treatments room to animate and the glow room for its aura.
const ROOMY: (u32, u32) = (760, 420);

/// A window that hugs each treatment's panel at full extent.
///
/// Widths come from what the frontend actually sets: 620 for the ribbons glass,
/// 560 for the glow shell, 470 for the minimal card. The extra is breathing
/// room for shadow and, in the glow's case, as much of the aura as can be kept
/// without giving the margin back.
fn tight(visual: &str) -> (u32, u32) {
    match visual {
        "minimal" => (500, 340),
        // The aura is a wide blurred halo around a 560px box. Trimming much
        // further clips it, so this treatment gains least from the workaround.
        "glow" => (700, 400),
        _ => (640, 380),
    }
}

/// Size the overlay window, then re-centre it.
///
/// `tight_fit` is the experimental workaround for tools that decorate windows —
/// Windhawk's translucent-windows mod is the one that prompted it. Those tools
/// paint the *window rectangle*, and because Verba's window is a large mostly
/// transparent canvas, the decoration appears as a panel floating around the
/// visible box.
///
/// Nothing inside this process stops that. Measured against the mod on a
/// machine running it: declaring `DWMSBT_NONE` made it worse, an
/// `ACCENT_DISABLED` composition attribute did nothing at show time or after,
/// `WS_EX_LAYERED` did nothing, and a window region did not clip it. What does
/// help is making the rectangle smaller, because the decoration tracks it — the
/// surrounding band went from about 90 physical pixels a side to under ten.
///
/// It is a mitigation, not a cure. The dependable fix is to exclude Verba in
/// the offending tool's own per-program rules, which the settings text says.
pub fn fit(win: &WebviewWindow, visual: &str, tight_fit: bool) -> Result<()> {
    let (w, h) = if tight_fit { tight(visual) } else { ROOMY };
    win.set_size(LogicalSize::new(w, h))?;
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
