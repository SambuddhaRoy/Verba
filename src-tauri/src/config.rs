//! User settings, persisted to %APPDATA%\Verba\config.json.
//!
//! JSON rather than TOML: this file is edited through the settings window, not
//! by hand, and serde_json is already in the tree via Tauri.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    pub size_mb: u32,
    pub note: &'static str,
    pub url: String,
    pub installed: bool,
    /// Rough VRAM or RAM needed to run it comfortably.
    pub needs_mb: u32,
}

const CATALOGUE: &[(&str, &str, u32, u32, &str)] = &[
    ("ggml-tiny.en-q5_1.bin", "Tiny (English)", 31, 400,
     "Fastest. Noticeably weaker on names and technical terms."),
    ("ggml-base.en-q5_1.bin", "Base (English)", 57, 600,
     "Quick everywhere. Fine for short, plain dictation."),
    ("ggml-small.en-q5_1.bin", "Small (English)", 181, 1100,
     "Best accuracy per megabyte. The sensible default."),
    ("ggml-medium.en-q5_0.bin", "Medium (English)", 514, 2600,
     "Better on jargon and accents. Wants a GPU."),
    ("ggml-large-v3-turbo-q5_0.bin", "Large v3 Turbo", 547, 3200,
     "Near-large accuracy with a four-layer decoder. Multilingual."),
];

pub fn catalogue() -> Vec<ModelInfo> {
    let dir = models_dir();
    CATALOGUE
        .iter()
        .map(|(file, name, size_mb, needs_mb, note)| ModelInfo {
            file: (*file).into(),
            name: (*name).into(),
            size_mb: *size_mb,
            needs_mb: *needs_mb,
            note,
            url: format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}"),
            installed: dir.join(file).exists(),
        })
        .collect()
}
