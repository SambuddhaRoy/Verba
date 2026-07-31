//! User settings, persisted to %APPDATA%\Verba\config.json.
//!
//! JSON rather than TOML: this file is edited through the settings window, not
//! by hand, and serde_json is already in the tree via Tauri.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Windows virtual-key codes we accept as the main key of a hotkey.
pub const VK_SPACE: u32 = 0x20;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Hotkey {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    /// Virtual-key code of the non-modifier key.
    pub vk: u32,
    /// What to draw on the keycap, e.g. "Space".
    pub label: String,
}

impl Default for Hotkey {
    fn default() -> Self {
        // Ctrl+Shift+Space is clear of anything Windows reserves, but it is
        // Trigger Parameter Hints in VS Code and Visual Studio — and the hook
        // swallows it, so that shortcut stops working while Verba runs. Being
        // reconfigurable is the point of this type.
        Self { ctrl: true, shift: true, alt: false, win: false, vk: VK_SPACE, label: "Space".into() }
    }
}

impl Hotkey {
    /// Packed modifier bits, for the atomics the keyboard hook reads.
    pub fn mods(&self) -> u32 {
        (self.ctrl as u32) | (self.shift as u32) << 1 | (self.alt as u32) << 2 | (self.win as u32) << 3
    }
}

#[derive(Serialize, Clone)]
pub struct EngineInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub available: bool,
    /// True if Verba can set this up itself from the settings window.
    pub installable: bool,
    pub note: &'static str,
}

pub fn engines() -> Vec<EngineInfo> {
    let have = available_engines();
    let can_install = installable_engines();
    [
        ("whisper.cpp", "whisper.cpp",
         "Compiled in. Reaches any GPU through Vulkan and needs no Python."),
        ("faster-whisper", "faster-whisper",
         "CTranslate2, run as a Python sidecar Verba installs for you. GPU path is CUDA-only — on AMD or Intel graphics it runs on CPU."),
        ("parakeet", "Parakeet / sherpa-onnx",
         "NVIDIA Parakeet and Moonshine, run through prebuilt sherpa-onnx wheels Verba installs for you. Far faster than Whisper; offline rather than streaming."),
    ]
    .into_iter()
    .map(|(id, name, note)| EngineInfo {
        id,
        name,
        note,
        available: have.contains(&id),
        installable: can_install.contains(&id),
    })
    .collect()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// "whisper.cpp" or "faster-whisper".
    pub engine: String,
    /// Model file name, resolved against the models directory.
    pub model: String,
    /// Input device name; None means the system default.
    pub microphone: Option<String>,
    pub language: String,
    pub launch_at_startup: bool,
    /// Load the model at startup rather than on first dictation. Costs memory
    /// while idle, removes the first-use stall.
    pub preload_model: bool,
    /// Unload the model after this many seconds idle. 0 keeps it resident.
    pub model_idle_eject_secs: u64,
    /// Overlay treatment: "ribbons" or "glow".
    pub visual: String,
    /// None means physical cores - 2.
    pub threads: Option<i32>,
    pub use_gpu: bool,
    pub hotkey: Hotkey,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            engine: "whisper.cpp".into(),
            model: "ggml-small.en-q5_1.bin".into(),
            microphone: None,
            language: "en".into(),
            launch_at_startup: false,
            preload_model: true,
            // Ten minutes: long enough that normal bursts of dictation never
            // pay the reload, short enough that an idle machine gets its
            // memory back.
            model_idle_eject_secs: 600,
            visual: "ribbons".into(),
            threads: None,
            use_gpu: true,
            hotkey: Hotkey::default(),
        }
    }
}

pub fn dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("Verba")
}

pub fn path() -> PathBuf {
    dir().join("config.json")
}

/// Never fails: a corrupt or missing file falls back to defaults, because
/// refusing to start over a bad settings file is worse than ignoring it.
pub fn load() -> Config {
    match std::fs::read_to_string(path()) {
        Ok(s) => {
            // Strip a UTF-8 BOM. serde_json rejects one outright, and plenty of
            // Windows editors — Notepad, PowerShell's Set-Content — add it
            // silently. Without this the file parses as garbage and every
            // setting reverts to default with only a line in the log.
            let s = s.strip_prefix('\u{feff}').unwrap_or(&s);
            serde_json::from_str(s).unwrap_or_else(|e| {
                crate::log!("config unreadable, using defaults: {e}");
                Config::default()
            })
        }
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    std::fs::write(path(), serde_json::to_string_pretty(cfg)?)?;
    Ok(())
}

/// Reject a model id that is anything other than a plain name.
///
/// `Path::join` with an absolute argument discards the base entirely, so an
/// unvalidated id from config.json could point the engine at any directory on
/// the machine. Every caller resolves the id against `models_dir`, so the check
/// belongs here rather than being repeated at each of them.
pub fn safe_model_name(name: &str) -> Result<&str> {
    let bad = name.is_empty()
        || name.contains(['/', '\\', ':'])
        || name.split('.').any(|seg| seg == "" && name.starts_with('.'))
        || name == ".."
        || name.starts_with("..");
    if bad {
        anyhow::bail!("invalid model name: {name:?}");
    }
    Ok(name)
}

/// Where model files live: next to the .exe first, then the repo layout.
pub fn models_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let p = d.join("models");
            if p.is_dir() {
                return p;
            }
        }
    }
    for p in ["../models", "models"] {
        let p = PathBuf::from(p);
        if p.is_dir() {
            return p;
        }
    }
    dir().join("models")
}

#[derive(Serialize, Clone)]
pub struct ModelInfo {
    pub file: String,
    pub name: String,
    /// Which engine can load this. Models whose engine is not built are listed
    /// but not selectable, so the roadmap is visible without offering a choice
    /// that would silently fail.
    pub engine: &'static str,
    pub size_mb: u32,
    /// Rough VRAM or RAM needed to run it comfortably.
    pub needs_mb: u32,
    pub note: &'static str,
    pub license: &'static str,
    /// True if the model streams natively, rather than needing a full buffer.
    pub streaming: bool,
    pub url: String,
    pub installed: bool,
}

/// file, display name, engine, size MB, needs MB, streaming, licence, note
type Entry = (&'static str, &'static str, &'static str, u32, u32, bool, &'static str, &'static str);

const CATALOGUE: &[Entry] = &[
    // --- whisper.cpp, GGML format -----------------------------------------
    ("ggml-tiny.en-q5_1.bin", "Whisper Tiny (English)", "whisper.cpp", 31, 400, false, "MIT",
     "Fastest there is. Noticeably weaker on names and technical terms."),
    ("ggml-base.en-q5_1.bin", "Whisper Base (English)", "whisper.cpp", 57, 600, false, "MIT",
     "Quick on any machine. Fine for short, plain dictation."),
    ("ggml-small.en-q5_1.bin", "Whisper Small (English)", "whisper.cpp", 181, 1100, false, "MIT",
     "Best accuracy per megabyte. The sensible default on CPU."),
    ("ggml-small-q5_1.bin", "Whisper Small (multilingual)", "whisper.cpp", 181, 1200, false, "MIT",
     "Small, with 99 languages instead of English only."),
    ("ggml-medium.en-q5_0.bin", "Whisper Medium (English)", "whisper.cpp", 514, 2600, false, "MIT",
     "Better on jargon and accents. Wants GPU offload."),
    ("ggml-large-v3-turbo-q5_0.bin", "Whisper Large v3 Turbo", "whisper.cpp", 547, 3200, false, "MIT",
     "Four-layer decoder: near-large accuracy at a fraction of the cost. Multilingual."),
    ("ggml-large-v3-turbo.bin", "Whisper Large v3 Turbo (f16)", "whisper.cpp", 1549, 4200, false, "MIT",
     "Unquantised Turbo. Marginal gain over q5 unless you have VRAM to spare."),
    ("ggml-large-v3-q5_0.bin", "Whisper Large v3", "whisper.cpp", 1031, 4600, false, "MIT",
     "Most accurate Whisper. Roughly 6x slower than Turbo for a small gain."),

    // --- faster-whisper, CTranslate2 --------------------------------------
    // Named by Hugging Face id, not a file: faster-whisper fetches and caches
    // its own weights on first use, so there is nothing here to download.
    ("tiny.en", "FW Tiny (English)", "faster-whisper", 75, 500, false, "MIT",
     "Fetched automatically on first use. Fastest, least accurate."),
    ("base.en", "FW Base (English)", "faster-whisper", 145, 700, false, "MIT",
     "Fetched automatically on first use."),
    ("small.en", "FW Small (English)", "faster-whisper", 484, 1400, false, "MIT",
     "Fetched automatically on first use. The usual default."),
    ("medium.en", "FW Medium (English)", "faster-whisper", 1530, 3000, false, "MIT",
     "Fetched automatically on first use. Wants a CUDA GPU."),
    ("large-v3", "FW Large v3", "faster-whisper", 3090, 5000, false, "MIT",
     "Fetched automatically on first use. CUDA strongly recommended."),
    ("distil-large-v3", "FW Distil-Large v3", "faster-whisper", 1510, 3000, false, "MIT",
     "Distilled: close to Large v3 accuracy at roughly half the size. English."),

    // --- sherpa-onnx: Parakeet and Moonshine ------------------------------
    // Names are the release archive stems, which are also the directory names
    // after extraction. These are ONNX, so whisper.cpp cannot load them.
    // All offline: sherpa-onnx does publish genuinely streaming Parakeet
    // builds, but those use a different recogniser that is not wired up.
    ("sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
     "Parakeet TDT 110M", "parakeet", 103, 700, false, "CC-BY-4.0",
     "Roughly 90x realtime on CPU alone — far faster than any Whisper build. English."),
    ("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
     "Parakeet TDT 0.6B v2", "parakeet", 460, 1800, false, "CC-BY-4.0",
     "Tops several English accuracy leaderboards and still runs many times faster than realtime."),
    ("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
     "Parakeet TDT 0.6B v3", "parakeet", 465, 1800, false, "CC-BY-4.0",
     "As v2, extended to 25 European languages."),
    ("sherpa-onnx-moonshine-tiny-en-int8",
     "Moonshine Tiny", "parakeet", 103, 500, false, "MIT",
     "Built for short utterances: cost scales with audio length, not a fixed window. English."),
    ("sherpa-onnx-moonshine-base-en-int8",
     "Moonshine Base", "parakeet", 239, 900, false, "MIT",
     "More accurate Moonshine, still very quick on CPU. English."),
];

/// sherpa-onnx publishes its models as release archives on one tag.
const SHERPA_RELEASE: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

#[cfg(test)]
mod tests {
    use super::*;

    /// A BOM on the config used to reset every setting to default with nothing
    /// but a log line, which is indistinguishable from the app ignoring you.
    /// `Path::join` with an absolute argument discards the base, so a model id
    /// carrying a separator escapes the models directory entirely.
    #[test]
    fn model_names_with_separators_are_rejected() {
        assert!(safe_model_name("ggml-small.en-q5_1.bin").is_ok());
        assert!(safe_model_name("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8").is_ok());

        for bad in [
            "",
            "..",
            "../secrets",
            r"..\secrets",
            r"C:\Windows\System32",
            "sub/dir",
            r"sub\dir",
        ] {
            assert!(safe_model_name(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn config_parses_with_and_without_bom() {
        let json = r#"{"engine":"parakeet","model":"x"}"#;
        let plain: Config = serde_json::from_str(json).unwrap();
        assert_eq!(plain.engine, "parakeet");

        let with_bom = format!("\u{feff}{json}");
        assert!(
            serde_json::from_str::<Config>(&with_bom).is_err(),
            "if serde starts accepting a BOM the strip below is dead code"
        );
        let stripped = with_bom.strip_prefix('\u{feff}').unwrap();
        let parsed: Config = serde_json::from_str(stripped).unwrap();
        assert_eq!(parsed.engine, "parakeet");
        assert_eq!(parsed.model, "x");
    }
}

/// Engines this build can actually run right now.
pub fn available_engines() -> Vec<&'static str> {
    // whisper.cpp is compiled in. faster-whisper depends on a Python
    // environment that Verba creates on demand, so it becomes available only
    // once that exists. Parakeet needs a sherpa-onnx backend that is not built.
    let mut v = vec!["whisper.cpp"];
    if crate::fasterwhisper::is_installed() {
        v.push("faster-whisper");
    }
    if crate::parakeet::is_installed() {
        v.push("parakeet");
    }
    v
}

/// Engines that can be installed from within the app.
pub fn installable_engines() -> Vec<&'static str> {
    vec!["faster-whisper", "parakeet"]
}

pub fn catalogue() -> Vec<ModelInfo> {
    let dir = models_dir();
    CATALOGUE
        .iter()
        .map(|(file, name, engine, size_mb, needs_mb, streaming, license, note)| ModelInfo {
            file: (*file).into(),
            name: (*name).into(),
            engine,
            size_mb: *size_mb,
            needs_mb: *needs_mb,
            note,
            license,
            streaming: *streaming,
            url: match *engine {
                "whisper.cpp" => {
                    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}")
                }
                "parakeet" => format!("{SHERPA_RELEASE}/{file}.tar.bz2"),
                // faster-whisper fetches its own weights from Hugging Face.
                _ => String::new(),
            },
            installed: match *engine {
                "whisper.cpp" => dir.join(file).exists(),
                // A directory of ONNX files; tokens.txt is the one member every
                // sherpa-onnx layout has.
                "parakeet" => dir.join(file).join("tokens.txt").is_file(),
                // Nothing to install per-model: it downloads on first use.
                "faster-whisper" => crate::fasterwhisper::is_installed(),
                _ => false,
            },
        })
        .collect()
}
