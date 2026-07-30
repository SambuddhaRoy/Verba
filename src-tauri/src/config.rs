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
    pub note: &'static str,
}

pub fn engines() -> Vec<EngineInfo> {
    let have = available_engines();
    [
        ("whisper.cpp", "whisper.cpp",
         "Reaches any GPU through Vulkan and needs no Python. Batch, not streaming."),
        ("parakeet", "Parakeet / sherpa-onnx",
         "NVIDIA Parakeet and Moonshine. Streams natively, so text appears while you speak. Needs a sherpa-onnx backend."),
        ("faster-whisper", "faster-whisper",
         "CTranslate2. Needs a Python runtime, and its GPU path is CUDA-only — no Vulkan, no Intel graphics."),
    ]
    .into_iter()
    .map(|(id, name, note)| EngineInfo { id, name, note, available: have.contains(&id) })
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
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            crate::log!("config unreadable, using defaults: {e}");
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    std::fs::write(path(), serde_json::to_string_pretty(cfg)?)?;
    Ok(())
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

    // --- NVIDIA Parakeet, ONNX via sherpa-onnx ----------------------------
    // Not GGML, so whisper.cpp cannot load these at all. Listed because
    // Parakeet is the streaming path and worth knowing about.
    ("sherpa-parakeet-tdt-0.6b-v3", "NVIDIA Parakeet TDT 0.6B v3", "parakeet", 640, 2400, true, "CC-BY-4.0",
     "Token-and-duration transducer. Streams natively and is far faster than realtime. 25 European languages."),
    ("sherpa-parakeet-tdt-0.6b-v2", "NVIDIA Parakeet TDT 0.6B v2", "parakeet", 620, 2400, true, "CC-BY-4.0",
     "English only. Tops several accuracy leaderboards while staying streaming-capable."),
    ("sherpa-parakeet-tdt_ctc-110m", "NVIDIA Parakeet TDT-CTC 110M", "parakeet", 120, 700, true, "CC-BY-4.0",
     "Small enough for a thin laptop and still streaming. English."),
    ("sherpa-nemo-canary-180m-flash", "NVIDIA Canary 180M Flash", "parakeet", 190, 900, false, "CC-BY-4.0",
     "Transcribes and translates across four languages. Batch, not streaming."),

    // --- Moonshine, ONNX --------------------------------------------------
    ("sherpa-moonshine-base", "Moonshine Base", "parakeet", 160, 700, true, "MIT",
     "Built for short utterances; cost scales with audio length rather than a fixed window."),
    ("sherpa-moonshine-tiny", "Moonshine Tiny", "parakeet", 60, 400, true, "MIT",
     "Very fast on CPU. English, short-form."),
];

/// Engines this build can actually run.
pub fn available_engines() -> Vec<&'static str> {
    // Only whisper.cpp is wired up. Parakeet needs a sherpa-onnx backend and
    // faster-whisper needs a Python sidecar; both are listed in the UI as
    // unavailable rather than pretended into existence.
    vec!["whisper.cpp"]
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
            url: if *engine == "whisper.cpp" {
                format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}")
            } else {
                // sherpa-onnx publishes these as release archives, not single files.
                "https://github.com/k2-fsa/sherpa-onnx/releases".into()
            },
            // Only meaningful for single-file GGML models; ONNX models are
            // directories, and nothing loads them yet regardless.
            installed: *engine == "whisper.cpp" && dir.join(file).exists(),
        })
        .collect()
}
