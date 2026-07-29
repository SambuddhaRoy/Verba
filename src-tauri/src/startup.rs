//! "Launch at startup", via the per-user Run key.
//!
//! HKCU, not HKLM: this is one application's own preference for one user, set
//! from that application's settings window. It needs no elevation and touches
//! nothing outside the user's own profile.

use anyhow::{anyhow, Result};
use windows::core::{w, HSTRING};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ,
};

const VALUE: &str = "Verba";

fn run_key() -> Result<HKEY> {
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
        .ok()
        .map_err(|e| anyhow!("open Run key: {e}"))?;
    }
    Ok(key)
}

pub fn set(enabled: bool) -> Result<()> {
    let key = run_key()?;
    let name = HSTRING::from(VALUE);
    let result = unsafe {
        if enabled {
            let exe = std::env::current_exe()?;
            // Quoted: the path contains spaces on most installs, and an
            // unquoted Run value silently launches the wrong thing.
            let cmd = HSTRING::from(format!("\"{}\"", exe.display()));
            let bytes: &[u8] = std::slice::from_raw_parts(
                cmd.as_ptr() as *const u8,
                (cmd.len() + 1) * 2, // include the terminating NUL
            );
            RegSetValueExW(key, &name, None, REG_SZ, Some(bytes))
                .ok()
                .map_err(|e| anyhow!("write Run value: {e}"))
        } else {
            // Absent is the desired state either way, so a missing value is
            // success, not an error.
            let _ = RegDeleteValueW(key, &name);
            Ok(())
        }
    };
    unsafe { let _ = RegCloseKey(key); }
    result
}
