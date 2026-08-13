//! The user's Windows accent colour.
//!
//! `UISettings` is the same source Windows uses for its own chrome, and it
//! gives the light and dark variants alongside the base colour — which matters
//! because a lot of accents are too dark to read as text on a dark surface.
//! The DWM registry value is the fallback: it is one DWORD and always present,
//! but it carries only the base colour.

use serde::Serialize;
use windows::UI::ViewManagement::{UIColorType, UISettings};

#[derive(Serialize, Clone, Debug)]
pub struct Accent {
    /// Base accent, for fills.
    pub base: String,
    /// Lighter variants. On a dark surface `light2` is what Windows itself
    /// uses for accent-coloured text.
    pub light1: String,
    pub light2: String,
    pub light3: String,
    pub dark1: String,
    /// Base as "r, g, b" so CSS can build rgba() at arbitrary alpha —
    /// `color-mix` would work too but this needs no fallback.
    pub rgb: String,
    /// "light" or "dark", from the same app-theme setting Windows uses for its
    /// own surfaces. Carried alongside the accent because the two change
    /// together — switching theme also swaps which accent variant is legible.
    pub theme: &'static str,
}

impl PartialEq for Accent {
    /// Only the values that reach CSS. Used by the watcher to decide whether
    /// anything actually changed, so it must not consider anything the UI
    /// cannot see.
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
            && self.light1 == other.light1
            && self.light2 == other.light2
            && self.light3 == other.light3
            && self.dark1 == other.dark1
            && self.rgb == other.rgb
            && self.theme == other.theme
    }
}

/// Whether Windows is in light or dark app mode.
///
/// `AppsUseLightTheme` rather than `SystemUsesLightTheme`: the first is the one
/// that governs application surfaces, which is what Verba's windows are. The
/// second controls the taskbar and Start, and users routinely set them apart.
/// Missing key means dark, which is Windows' own default and this app's.
fn theme() -> &'static str {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
        REG_VALUE_TYPE,
    };

    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        )
        .is_err()
        {
            return "dark";
        }

        let mut value = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let ok = RegQueryValueExW(
            key,
            w!("AppsUseLightTheme"),
            None,
            Some(&mut kind),
            Some(&mut value as *mut u32 as *mut u8),
            Some(&mut size),
        )
        .is_ok();
        let _ = RegCloseKey(key);

        if ok && value == 1 { "light" } else { "dark" }
    }
}

fn hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn from_ui_settings() -> Option<Accent> {
    let s = UISettings::new().ok()?;
    let get = |t| s.GetColorValue(t).ok().map(|c| (c.R, c.G, c.B));

    let (r, g, b) = get(UIColorType::Accent)?;
    let (l1r, l1g, l1b) = get(UIColorType::AccentLight1)?;
    let (l2r, l2g, l2b) = get(UIColorType::AccentLight2)?;
    let (l3r, l3g, l3b) = get(UIColorType::AccentLight3)?;
    let (d1r, d1g, d1b) = get(UIColorType::AccentDark1)?;

    Some(Accent {
        base: hex(r, g, b),
        light1: hex(l1r, l1g, l1b),
        light2: hex(l2r, l2g, l2b),
        light3: hex(l3r, l3g, l3b),
        dark1: hex(d1r, d1g, d1b),
        rgb: format!("{r}, {g}, {b}"),
        theme: theme(),
    })
}

/// Parse `Explorer\Accent\AccentPalette`: eight RGBA entries, lightest first.
///
/// Deliberately *not* `DWM\AccentColor`. That is the title-bar colour, a
/// separate setting that is often stale — on this machine it reads as blue
/// while the actual accent is green, and `ColorPrevalence` is 0 so Windows
/// never draws it. AccentPalette is the same data UISettings reports.
fn parse_palette(bytes: &[u8]) -> Option<Accent> {
    if bytes.len() < 32 {
        return None;
    }
    let at = |i: usize| (bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2]);
    let (r, g, b) = at(3); // index 3 is the base accent
    let (l1r, l1g, l1b) = at(2);
    let (l2r, l2g, l2b) = at(1);
    let (l3r, l3g, l3b) = at(0);
    let (d1r, d1g, d1b) = at(4);
    Some(Accent {
        base: hex(r, g, b),
        light1: hex(l1r, l1g, l1b),
        light2: hex(l2r, l2g, l2b),
        light3: hex(l3r, l3g, l3b),
        dark1: hex(d1r, d1g, d1b),
        rgb: format!("{r}, {g}, {b}"),
        theme: theme(),
    })
}

fn from_registry() -> Option<Accent> {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
        REG_VALUE_TYPE,
    };

    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        )
        .ok()
        .ok()?;

        let mut buf = [0u8; 32];
        let mut size = buf.len() as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let ok = RegQueryValueExW(
            key,
            w!("AccentPalette"),
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        )
        .is_ok();
        let _ = RegCloseKey(key);

        if ok { parse_palette(&buf) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_palette;

    /// Real bytes from a machine whose accent is green, checked against what
    /// UISettings reported on the same machine. Index 3 is the base — reading
    /// index 0 would give the lightest tint and look washed out.
    #[test]
    fn palette_indices_match_uisettings() {
        let mut b = [0u8; 32];
        for (i, rgb) in [
            [0xA8, 0xE9, 0xBA], [0x85, 0xCD, 0x99], [0x42, 0xA1, 0x5E],
            [0x36, 0x83, 0x4D], [0x2A, 0x6B, 0x3C], [0x1B, 0x4D, 0x27],
            [0x0A, 0x2A, 0x0E], [0x88, 0x17, 0x98],
        ]
        .iter()
        .enumerate()
        {
            b[i * 4..i * 4 + 3].copy_from_slice(rgb);
        }
        let a = parse_palette(&b).unwrap();
        assert_eq!(a.base, "#36834D");
        assert_eq!(a.light2, "#85CD99");
        assert_eq!(a.dark1, "#2A6B3C");
        assert_eq!(a.rgb, "54, 131, 77");
    }

    #[test]
    fn short_palette_is_rejected() {
        assert!(parse_palette(&[0u8; 8]).is_none());
    }
}

pub fn detect() -> Accent {
    from_ui_settings().or_else(from_registry).unwrap_or_else(|| Accent {
        // Windows' own default blue, for the case where neither source answers.
        base: "#0078D4".into(),
        light1: "#268CDE".into(),
        light2: "#4CA0E8".into(),
        light3: "#72B4F2".into(),
        dark1: "#005FAA".into(),
        rgb: "0, 120, 212".into(),
        theme: theme(),
    })
}
