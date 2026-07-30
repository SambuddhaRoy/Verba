//! Grab whatever is on screen behind the overlay.
//!
//! `backdrop-filter` cannot help here. In a transparent window it samples the
//! page's own compositing tree, and there is nothing behind the panel but the
//! desktop — which the webview cannot see. So the desktop is captured, scaled
//! down hard, and handed to the frontend as an image to blur.
//!
//! Downscaling *is* most of the blur: averaging 8x8 pixel blocks is a box blur
//! at a fraction of the cost, and the CSS blur on top only has to soften what
//! is left. It also keeps the payload at tens of kilobytes rather than megabytes.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HALFTONE, HBITMAP, SRCCOPY,
};

/// How much to shrink before sending. 8 keeps a 760x420 region at 95x53.
pub const SCALE: i32 = 8;

pub struct Shot {
    pub width: u32,
    pub height: u32,
    /// RGBA, top-down, ready for `ImageData`.
    pub rgba: Vec<u8>,
}

/// Capture the screen rectangle at `(x, y, w, h)` in physical pixels.
///
/// Call this *before* showing the overlay — otherwise the overlay is in the
/// shot and the panel ends up blurring an image of itself.
pub fn screen_region(x: i32, y: i32, w: i32, h: i32) -> Option<Shot> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let dw = (w / SCALE).max(1);
    let dh = (h / SCALE).max(1);

    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));

        // Negative height requests a top-down DIB, which matches the row order
        // ImageData expects and saves a flip.
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: dw,
            biHeight: -dh,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib: HBITMAP =
            match CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => b,
                Err(_) => {
                    let _ = DeleteDC(mem);
                    ReleaseDC(None, screen);
                    return None;
                }
            };
        let old = SelectObject(mem, dib.into());

        // HALFTONE averages when shrinking instead of dropping pixels, which is
        // what makes the downscale act as a blur rather than as aliasing.
        SetStretchBltMode(mem, HALFTONE);
        let ok = if dw == w && dh == h {
            BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok()
        } else {
            StretchBlt(mem, 0, 0, dw, dh, Some(screen), x, y, w, h, SRCCOPY).as_bool()
        };

        let shot = if ok && !bits.is_null() {
            let n = (dw * dh) as usize;
            let src = std::slice::from_raw_parts(bits as *const u8, n * 4);
            let mut rgba = Vec::with_capacity(n * 4);
            // GDI gives BGRA with an unused alpha byte; swap and force opaque.
            for px in src.chunks_exact(4) {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
            Some(Shot { width: dw as u32, height: dh as u32, rgba })
        } else {
            None
        };

        SelectObject(mem, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        shot
    }
}

/// Capture what is behind a window, using its own bounds.
pub fn behind(hwnd: HWND) -> Option<Shot> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r).ok()? };
    screen_region(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// Base64 without a dependency. One function, and the alternative is a crate
/// pulled in for twenty lines.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    /// Padding is where hand-rolled base64 goes wrong, so check every remainder.
    #[test]
    fn base64_matches_rfc4648_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
