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

/// A formatting treatment applied to a finished transcript.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Mode {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Sent to the local model as the system prompt when `llm` is on.
    pub instructions: String,
    /// Off means the deterministic rules alone, which cost nothing and never
    /// invent words. Worth defaulting to for anything where fidelity matters
    /// more than polish.
    pub llm: bool,
    pub strip_fillers: bool,
    pub spoken_punctuation: bool,
}

impl Default for Mode {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            instructions: String::new(),
            llm: false,
            strip_fillers: true,
            spoken_punctuation: true,
        }
    }
}

/// Route a mode by which application has focus.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AppRule {
    pub mode: String,
    /// Executable names, matched case-insensitively and exactly.
    pub exe: Vec<String>,
    /// Optional extra condition on the window title, matched as a
    /// case-insensitive substring. Plain substring rather than a regex: it
    /// covers "compose" and "re:" without a dependency, and a rule nobody can
    /// read is worse than one that matches slightly too much.
    pub title: Option<String>,
}

fn default_modes() -> Vec<Mode> {
    vec![
        Mode {
            id: "raw".into(),
            name: "Raw".into(),
            description: "No model pass. Punctuation and filler cleanup only.".into(),
            llm: false,
            ..Default::default()
        },
        Mode {
            id: "email".into(),
            name: "Email".into(),
            description: "Clean prose, no filler.".into(),
            // "Never add" is not enough on its own: a model asked to improve
            // prose will supply the judgement it thinks is implied. Observed
            // turning "the seat minimum reads like it applies to everyone"
            // into "...when it should not", which the speaker never said.
            instructions: "Rewrite this dictation as a clear email body. Fix grammar \
                           and punctuation, keep the speaker's voice and level of \
                           formality, and keep every point they made. Do not add \
                           opinions, conclusions or judgements they did not state, \
                           even if implied. Do not add a greeting or sign-off unless \
                           one was dictated. Output only the rewritten text."
                .into(),
            llm: true,
            ..Default::default()
        },
        Mode {
            id: "notes".into(),
            name: "Notes".into(),
            description: "Bulleted and terse; keeps names and numbers.".into(),
            // "Terse bullet points" alone reads as licence to summarise: this
            // returned a single seven-word bullet for a forty-word dictation.
            // The rule that fixed it is one bullet per point, nothing dropped.
            instructions: "Turn this dictation into bullet points. Every distinct point \
                           becomes its own bullet — do not summarise, merge or omit \
                           anything that was said. Tighten the wording, but keep all \
                           the content. Preserve every name, number, date and technical \
                           term exactly. Output only the bullets."
                .into(),
            llm: true,
            ..Default::default()
        },
        Mode {
            id: "chat".into(),
            name: "Chat".into(),
            description: "One or two sentences, no salutation, no sign-off.".into(),
            // Chat is not short email: a model given the email brief adds
            // openers and closings nobody types into Slack.
            instructions: "Clean up this dictation for a chat message. Fix grammar and \
                           punctuation and keep it conversational. Do not add a \
                           greeting, a sign-off, or any pleasantry that was not \
                           dictated. Do not expand it or make it more formal. Output \
                           only the message."
                .into(),
            llm: true,
            ..Default::default()
        },
        Mode {
            id: "code".into(),
            name: "Code".into(),
            description: "Identifiers verbatim; spoken symbols become syntax.".into(),
            instructions: "This dictation describes code. Keep every identifier exactly \
                           as spoken, including casing. Convert spoken symbols to their \
                           syntax. Do not explain, do not add code that was not \
                           described. Output only the result."
                .into(),
            llm: true,
            // Fillers are stripped, but spoken punctuation is left alone: a
            // developer saying "dot" or "colon" usually means the character,
            // and Stage 1 substituting it first would double up with the model.
            spoken_punctuation: false,
            ..Default::default()
        },
    ]
}

fn rule(mode: &str, exe: &[&str]) -> AppRule {
    AppRule { mode: mode.into(), exe: exe.iter().map(|s| s.to_string()).collect(), title: None }
}

fn default_rules() -> Vec<AppRule> {
    vec![
        // Terminals first and deliberately Raw: a shell command must reach the
        // prompt exactly as spoken, and any rewrite is a wrong command run.
        rule("raw", &[
            "WindowsTerminal.exe", "powershell.exe", "pwsh.exe", "cmd.exe",
            "conhost.exe", "wt.exe", "alacritty.exe", "wezterm-gui.exe", "mintty.exe",
        ]),
        rule("code", &[
            "Code.exe", "Code - Insiders.exe", "VSCodium.exe", "devenv.exe",
            "idea64.exe", "pycharm64.exe", "webstorm64.exe", "clion64.exe",
            "goland64.exe", "rider64.exe", "rustrover64.exe",
            "sublime_text.exe", "notepad++.exe", "zed.exe", "cursor.exe",
            "nvim-qt.exe", "gvim.exe",
        ]),
        rule("email", &[
            "olk.exe", "OUTLOOK.EXE", "thunderbird.exe", "HxOutlook.exe",
            "Mailbird.exe", "em Client.exe", "Spark.exe",
        ]),
        rule("chat", &[
            "slack.exe", "Teams.exe", "ms-teams.exe", "Discord.exe",
            "WhatsApp.exe", "Telegram.exe", "signal.exe", "Element.exe",
            "Zoom.exe", "Skype.exe",
        ]),
        rule("notes", &[
            "Obsidian.exe", "Notion.exe", "onenote.exe", "ONENOTE.EXE",
            "logseq.exe", "Typora.exe", "joplin.exe", "AnyType.exe",
            "Craft.exe", "Bear.exe",
        ]),
        // Long-form writing gets the email treatment: clean connected prose,
        // no bullets, no chat register.
        rule("email", &["WINWORD.EXE", "soffice.bin", "scrivener.exe", "Obsidian Publish.exe"]),
    ]
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// "whisper.cpp" or "faster-whisper".
    /// False until the first-run flow has been completed or skipped. Existing
    /// configs predate the field and deserialise to false, so an upgrade shows
    /// the flow once — which is the right outcome, since it is also where the
    /// rewrite model gets chosen.
    pub onboarded: bool,
    /// Check GitHub for a newer release and install it. On by default: a
    /// dictation tool that silently rots is worse than one that restarts
    /// itself occasionally, and the restart only ever happens while idle.
    pub auto_update: bool,
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

    // --- formatting -------------------------------------------------------
    /// Used when no app rule matches.
    pub default_mode: String,
    pub modes: Vec<Mode>,
    /// First match wins, so more specific rules belong earlier.
    pub rules: Vec<AppRule>,
    /// Terms to fix the casing of, or `spoken => written` pairs for words the
    /// recogniser reliably mangles.
    pub vocabulary: Vec<String>,
    /// Ollama endpoint and model for the rewrite pass.
    pub llm_url: String,
    pub llm_model: String,
    /// Words dropped before any model sees the text. Deliberately short and
    /// unambiguous — "like" and "so" are real words far more often than they
    /// are filler, and stripping them silently corrupts meaning.
    pub fillers: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            onboarded: false,
            auto_update: true,
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

            default_mode: "raw".into(),
            modes: default_modes(),
            rules: default_rules(),
            vocabulary: Vec::new(),
            llm_url: "http://127.0.0.1:11434".into(),
            // Deliberately empty rather than a guess. Four of the default modes
            // ask for a rewrite, so naming a model here would have every fresh
            // install fire a request for weights it does not have — a failure
            // that is not a transport error, so the unreachable cache never
            // suppresses it and every dictation pays for it. Empty means "no
            // rewrite"; onboarding and settings fill it in once a model exists.
            llm_model: String::new(),
            fillers: ["um", "uh", "erm", "uhm", "hmm", "mhm"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

impl Config {
    pub fn mode(&self, id: &str) -> Option<&Mode> {
        self.modes.iter().find(|m| m.id == id)
    }

    /// Which mode applies to the app that had focus, first matching rule wins.
    pub fn mode_for(&self, exe: &str, title: &str) -> &Mode {
        let matched = self.rules.iter().find(|r| {
            let exe_hit = r.exe.iter().any(|e| e.eq_ignore_ascii_case(exe));
            let title_hit = match &r.title {
                None => true,
                Some(t) => title.to_lowercase().contains(&t.to_lowercase()),
            };
            exe_hit && title_hit
        });

        matched
            .and_then(|r| self.mode(&r.mode))
            .or_else(|| self.mode(&self.default_mode))
            // A config naming a mode that no longer exists must still dictate;
            // falling back to the first defined mode beats refusing to insert.
            .or_else(|| self.modes.first())
            .unwrap_or_else(|| {
                static RAW: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
                RAW.get_or_init(|| Mode { id: "raw".into(), name: "Raw".into(), ..Default::default() })
            })
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
    /// 0-100 guidance for comparing rows, not a measurement. See `Entry`.
    pub accuracy: u8,
    /// The model's own speed rating, on hardware that can hold it.
    pub speed: u8,
    /// Speed re-rated for this machine — the same model is quick on a GPU and
    /// slow when it has to spill to CPU, and showing one number for both would
    /// recommend a large model to a laptop that will crawl under it.
    pub speed_here: u8,
    /// Whether this machine has the memory to run it at all.
    pub fits: bool,
    /// True if the model streams natively, rather than needing a full buffer.
    pub streaming: bool,
    pub url: String,
    pub installed: bool,
}

/// file, display name, engine, size MB, needs MB, streaming, licence, note,
/// accuracy, speed.
///
/// The last two are 0-100 guidance for comparing rows against each other, not
/// measurements: accuracy tracks published English word-error rates, speed
/// tracks throughput on hardware that can actually hold the model. They are
/// deliberately coarse, and `models_for()` re-rates speed for the machine in
/// front of you, because a large model is quick on a GPU and painful without
/// one. Their relative ordering is pinned by a test — a transposed pair here
/// would be invisible in the UI but would recommend the wrong model forever.
type Entry = (
    &'static str, &'static str, &'static str, u32, u32, bool, &'static str, &'static str, u8, u8,
);

const CATALOGUE: &[Entry] = &[
    // --- whisper.cpp, GGML format -----------------------------------------
    ("ggml-tiny.en-q5_1.bin", "Whisper Tiny (English)", "whisper.cpp", 31, 400, false, "MIT",
     "Fastest there is. Noticeably weaker on names and technical terms.", 55, 97),
    ("ggml-base.en-q5_1.bin", "Whisper Base (English)", "whisper.cpp", 57, 600, false, "MIT",
     "Quick on any machine. Fine for short, plain dictation.", 64, 92),
    ("ggml-small.en-q5_1.bin", "Whisper Small (English)", "whisper.cpp", 181, 1100, false, "MIT",
     "Best accuracy per megabyte. The sensible default on CPU.", 78, 80),
    ("ggml-small-q5_1.bin", "Whisper Small (multilingual)", "whisper.cpp", 181, 1200, false, "MIT",
     "Small, with 99 languages instead of English only.", 74, 78),
    ("ggml-medium.en-q5_0.bin", "Whisper Medium (English)", "whisper.cpp", 514, 2600, false, "MIT",
     "Better on jargon and accents. Wants GPU offload.", 87, 55),
    ("ggml-large-v3-turbo-q5_0.bin", "Whisper Large v3 Turbo", "whisper.cpp", 547, 3200, false, "MIT",
     "Four-layer decoder: near-large accuracy at a fraction of the cost. Multilingual.", 93, 68),
    ("ggml-large-v3-turbo.bin", "Whisper Large v3 Turbo (f16)", "whisper.cpp", 1549, 4200, false, "MIT",
     "Unquantised Turbo. Marginal gain over q5 unless you have VRAM to spare.", 94, 60),
    ("ggml-large-v3-q5_0.bin", "Whisper Large v3", "whisper.cpp", 1031, 4600, false, "MIT",
     "Most accurate Whisper. Roughly 6x slower than Turbo for a small gain.", 96, 30),

    // --- faster-whisper, CTranslate2 --------------------------------------
    // Named by Hugging Face id, not a file: faster-whisper fetches and caches
    // its own weights on first use, so there is nothing here to download.
    ("tiny.en", "FW Tiny (English)", "faster-whisper", 75, 500, false, "MIT",
     "Fetched automatically on first use. Fastest, least accurate.", 55, 98),
    ("base.en", "FW Base (English)", "faster-whisper", 145, 700, false, "MIT",
     "Fetched automatically on first use.", 64, 94),
    ("small.en", "FW Small (English)", "faster-whisper", 484, 1400, false, "MIT",
     "Fetched automatically on first use. The usual default.", 78, 85),
    ("medium.en", "FW Medium (English)", "faster-whisper", 1530, 3000, false, "MIT",
     "Fetched automatically on first use. Wants a CUDA GPU.", 87, 62),
    ("large-v3", "FW Large v3", "faster-whisper", 3090, 5000, false, "MIT",
     "Fetched automatically on first use. CUDA strongly recommended.", 96, 38),
    ("distil-large-v3", "FW Distil-Large v3", "faster-whisper", 1510, 3000, false, "MIT",
     "Distilled: close to Large v3 accuracy at roughly half the size. English.", 92, 70),

    // --- sherpa-onnx: Parakeet and Moonshine ------------------------------
    // Names are the release archive stems, which are also the directory names
    // after extraction. These are ONNX, so whisper.cpp cannot load them.
    // All offline: sherpa-onnx does publish genuinely streaming Parakeet
    // builds, but those use a different recogniser that is not wired up.
    ("sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
     "Parakeet TDT 110M", "parakeet", 103, 700, false, "CC-BY-4.0",
     "Roughly 90x realtime on CPU alone — far faster than any Whisper build. English.", 76, 99),
    ("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
     "Parakeet TDT 0.6B v2", "parakeet", 460, 1800, false, "CC-BY-4.0",
     "Tops several English accuracy leaderboards and still runs many times faster than realtime.", 95, 90),
    ("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
     "Parakeet TDT 0.6B v3", "parakeet", 465, 1800, false, "CC-BY-4.0",
     "As v2, extended to 25 European languages.", 94, 88),
    ("sherpa-onnx-moonshine-tiny-en-int8",
     "Moonshine Tiny", "parakeet", 103, 500, false, "MIT",
     "Built for short utterances: cost scales with audio length, not a fixed window. English.", 60, 98),
    ("sherpa-onnx-moonshine-base-en-int8",
     "Moonshine Base", "parakeet", 239, 900, false, "MIT",
     "More accurate Moonshine, still very quick on CPU. English.", 71, 94),
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

/// The catalogue with no machine in mind. `speed_here` is the model's own
/// rating and `fits` is optimistic; use `catalogue_for()` wherever hardware is
/// known, which is everywhere the user actually sees this.
pub fn catalogue() -> Vec<ModelInfo> {
    catalogue_with(None)
}

/// The catalogue rated for one machine: speed adjusted for whether the model
/// can be held on the GPU, and `fits` set from real memory.
pub fn catalogue_for(hw: &crate::hardware::Hardware) -> Vec<ModelInfo> {
    catalogue_with(Some(hw))
}

fn catalogue_with(hw: Option<&crate::hardware::Hardware>) -> Vec<ModelInfo> {
    let dir = models_dir();
    CATALOGUE
        .iter()
        .map(|(file, name, engine, size_mb, needs_mb, streaming, license, note, accuracy, speed)| ModelInfo {
            file: (*file).into(),
            name: (*name).into(),
            engine,
            size_mb: *size_mb,
            needs_mb: *needs_mb,
            note,
            license,
            streaming: *streaming,
            accuracy: *accuracy,
            speed: *speed,
            speed_here: match hw {
                Some(hw) => crate::hardware::speed_on(hw, *needs_mb, *speed),
                None => *speed,
            },
            fits: match hw {
                Some(hw) => crate::hardware::fits(hw, *needs_mb),
                None => true,
            },
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
