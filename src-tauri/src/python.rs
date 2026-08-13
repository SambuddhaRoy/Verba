//! Finding a real Python, and installing one if there is none.
//!
//! `Command::new("python")` is not good enough on Windows. Since Windows 10,
//! `%LOCALAPPDATA%\Microsoft\WindowsApps` contains zero-byte reparse points
//! called `python.exe` and `python3.exe` — App Execution Aliases that open the
//! Microsoft Store instead of running anything. They are on PATH by default, so
//! on a machine with no Python at all, spawning "python" does not fail with
//! "not found": it either launches the Store or exits with a message about an
//! app not being installed. That is what the engine installer used to surface
//! as a one-line error before quietly leaving the user on whisper.cpp.
//!
//! So: resolve Python properly, reject the stubs, check the version, and if
//! there really is none, say so plainly and offer to install it.

use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// faster-whisper and sherpa-onnx both publish wheels from 3.9 onwards, and
/// below that pip resolves to ancient versions that fail at import.
const MIN: (u32, u32) = (3, 9);

/// What winget is asked for when the user opts in. Pinned to a series rather
/// than "latest": a brand-new major release routinely has no wheels yet for
/// exactly the packages this is for.
const WINGET_ID: &str = "Python.Python.3.12";

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Status {
    /// A usable interpreter.
    Ok { path: String, version: String },
    /// Present but too old to install the engines against.
    TooOld { path: String, version: String, needs: String },
    /// Nothing but the Microsoft Store aliases. Worth distinguishing from
    /// Missing, because the user very reasonably believes Python is installed —
    /// typing `python` in a terminal does *something*.
    StoreStubOnly,
    Missing,
}

impl Status {
    pub fn usable(&self) -> bool {
        matches!(self, Status::Ok { .. })
    }
}

/// A zero-byte reparse point under WindowsApps: an App Execution Alias, not an
/// interpreter. Checked by length and location rather than by running it,
/// because running it is precisely what opens the Store.
pub fn is_store_stub(path: &Path) -> bool {
    let looks_like_alias = path
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("WindowsApps"));
    if !looks_like_alias {
        return false;
    }
    // The alias is a zero-length file. A real interpreter that happened to live
    // under WindowsApps — a Store-installed Python — has a real size, and must
    // not be rejected.
    std::fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true)
}

/// Parse `sys.version_info` as printed by `print(sys.version_info[0], sys.version_info[1])`.
fn parse_version(out: &str) -> Option<(u32, u32)> {
    let mut it = out.split_whitespace();
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// Ask an interpreter what it is. `None` if it will not run at all.
fn interrogate(exe: &Path) -> Option<(u32, u32)> {
    if is_store_stub(exe) {
        return None;
    }
    let out = crate::childguard::hidden(Command::new(exe))
        .args(["-c", "import sys; print(sys.version_info[0], sys.version_info[1])"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// Every place worth looking, best first.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    // The launcher is the most reliable: it knows about every registered
    // install and is never an alias.
    if let Ok(o) = crate::childguard::hidden(Command::new("py"))
        .args(["-3", "-c", "import sys; print(sys.executable)"])
        .output()
    {
        if o.status.success() {
            let p = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
            if p.is_file() {
                out.push(p);
            }
        }
    }

    // PATH, minus the aliases.
    for name in ["python", "python3"] {
        if let Ok(o) = crate::childguard::hidden(Command::new("where"))
            .arg(name)
            .output()
        {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let p = PathBuf::from(line.trim());
                if p.is_file() && !is_store_stub(&p) {
                    out.push(p);
                }
            }
        }
    }

    // Default install locations, for a Python installed without "add to PATH" —
    // which is the installer's own default, so this is common rather than exotic.
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("Programs").join("Python"));
    }
    roots.push(PathBuf::from("C:\\"));
    if let Ok(pf) = std::env::var("ProgramFiles") {
        roots.push(PathBuf::from(pf));
    }
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.to_ascii_lowercase().starts_with("python") {
                let exe = e.path().join("python.exe");
                if exe.is_file() {
                    out.push(exe);
                }
            }
        }
    }

    out.dedup();
    out
}

/// The best interpreter on this machine, and what is wrong if there is none.
pub fn status() -> Status {
    let mut best_old: Option<(PathBuf, (u32, u32))> = None;

    for exe in candidates() {
        let Some(v) = interrogate(&exe) else { continue };
        if v >= MIN {
            return Status::Ok {
                path: exe.display().to_string(),
                version: format!("{}.{}", v.0, v.1),
            };
        }
        if best_old.as_ref().is_none_or(|(_, bv)| v > *bv) {
            best_old = Some((exe, v));
        }
    }

    if let Some((exe, v)) = best_old {
        return Status::TooOld {
            path: exe.display().to_string(),
            version: format!("{}.{}", v.0, v.1),
            needs: format!("{}.{}", MIN.0, MIN.1),
        };
    }

    // Nothing ran. Distinguish "the aliases are there and the user thinks they
    // have Python" from "there is genuinely nothing".
    let stubbed = std::env::var("LOCALAPPDATA").is_ok_and(|l| {
        let p = PathBuf::from(l)
            .join("Microsoft")
            .join("WindowsApps")
            .join("python.exe");
        p.exists()
    });
    if stubbed {
        Status::StoreStubOnly
    } else {
        Status::Missing
    }
}

/// A usable interpreter, or an error that says what to do about it.
pub fn require() -> Result<PathBuf> {
    match status() {
        Status::Ok { path, .. } => Ok(PathBuf::from(path)),
        Status::TooOld { version, needs, .. } => bail!(
            "Python {version} is too old — this engine needs {needs} or newer. \
             Install a current Python and try again."
        ),
        Status::StoreStubOnly => bail!(
            "Python is not installed. Windows ships a placeholder that opens the \
             Microsoft Store, which is why typing `python` appears to do something. \
             Install Python from Settings, or from python.org, then try again."
        ),
        Status::Missing => bail!(
            "Python is not installed, and this engine runs as a Python sidecar. \
             Install it from Settings, or from python.org, then try again."
        ),
    }
}

/// Is there a package manager that can install Python without a browser?
pub fn winget_available() -> bool {
    crate::childguard::hidden(Command::new("winget"))
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Install Python via winget, reporting progress lines as they arrive.
///
/// Deliberately not silent: this installs software on the user's machine, so it
/// only ever runs from an explicit button press, and the engine installer says
/// what is missing rather than doing this behind their back.
pub fn install<F: FnMut(&str)>(mut report: F) -> Result<()> {
    if !winget_available() {
        bail!(
            "winget is not available on this machine. Install Python from \
             python.org, then try again."
        );
    }

    report("installing Python via winget… this can take a few minutes");
    let out = crate::childguard::hidden(Command::new("winget"))
        .args([
            "install",
            "--id",
            WINGET_ID,
            "--exact",
            "--source",
            "winget",
            "--accept-package-agreements",
            "--accept-source-agreements",
            // Without this winget can sit waiting on a UAC prompt nobody sees.
            "--disable-interactivity",
        ])
        .output()
        .map_err(|e| anyhow!("could not run winget: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        bail!("winget could not install Python: {}", detail.trim());
    }

    // winget reports success before the new PATH reaches this already-running
    // process, so verify by looking on disk rather than trusting the exit code.
    report("verifying…");
    match status() {
        Status::Ok { version, .. } => {
            report(&format!("Python {version} installed"));
            Ok(())
        }
        _ => bail!(
            "winget reported success but no usable Python was found. \
             A restart may be needed for it to appear."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole reason this module exists. A zero-byte alias under WindowsApps
    /// must never be treated as an interpreter, and a real Python must never be
    /// rejected for living in an unusual place.
    #[test]
    fn store_aliases_are_rejected_and_real_pythons_are_not() {
        assert!(!is_store_stub(Path::new("C:\\Python312\\python.exe")));
        assert!(!is_store_stub(Path::new(
            "C:\\Users\\x\\AppData\\Local\\Programs\\Python\\Python312\\python.exe"
        )));

        // Non-existent path under WindowsApps: treated as a stub, since we
        // cannot prove otherwise and running it is the thing to avoid.
        assert!(is_store_stub(Path::new(
            "C:\\Users\\x\\AppData\\Local\\Microsoft\\WindowsApps\\python.exe"
        )));
        // Matching is on a path component, so a directory merely containing the
        // word must not trigger it.
        assert!(!is_store_stub(Path::new("C:\\MyWindowsAppsBackup\\python.exe")));
    }

    /// The real stubs on this machine, if present, have to be recognised —
    /// this is the case that made the old error message so confusing.
    #[test]
    fn the_actual_aliases_on_this_machine_are_rejected() {
        let Ok(local) = std::env::var("LOCALAPPDATA") else { return };
        let alias = PathBuf::from(local)
            .join("Microsoft")
            .join("WindowsApps")
            .join("python.exe");
        if alias.exists() {
            assert!(
                is_store_stub(&alias),
                "{} is an App Execution Alias and must be rejected",
                alias.display()
            );
            assert!(interrogate(&alias).is_none(), "the alias must never be run");
        }
    }

    #[test]
    fn versions_parse() {
        assert_eq!(parse_version("3 12\n"), Some((3, 12)));
        assert_eq!(parse_version("3 9"), Some((3, 9)));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("not a version"), None);
        assert_eq!(parse_version("3"), None);
    }

    /// Ordering has to be numeric, or 3.10 reads as older than 3.9 and a
    /// perfectly good interpreter is rejected as too old.
    #[test]
    fn version_comparison_is_numeric() {
        assert!((3u32, 10u32) > (3, 9));
        assert!((3u32, 12u32) >= MIN);
        assert!((3u32, 8u32) < MIN);
        assert!((2u32, 7u32) < MIN);
    }
}
