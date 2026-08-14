//! Finding, starting and stocking the local Ollama server.
//!
//! Post-processing is worthless if the server is not up, and asking the user
//! to go and start it is a poor answer when Verba can do it. So: locate the
//! binary, start it if it is installed and idle, and say plainly what to do if
//! it is not installed at all.
//!
//! Verba never *stops* Ollama. It is a shared background service the user may
//! be using from other tools, and shutting it down on quit would be
//! presumptuous in a way that starting it is not.

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where the Windows installer puts it, then anything on PATH.
pub fn exe() -> Option<PathBuf> {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = PathBuf::from(local).join(r"Programs\Ollama\ollama.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    for root in ["ProgramFiles", "ProgramW6432"] {
        if let Ok(dir) = std::env::var(root) {
            let p = PathBuf::from(dir).join(r"Ollama\ollama.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    // On PATH but installed somewhere unusual.
    Command::new("where")
        .arg("ollama")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|l| PathBuf::from(l.trim()))
        })
        .filter(|p| p.is_file())
}

pub fn is_running(cfg: &Config) -> bool {
    crate::net::get(&format!("{}/api/tags", cfg.llm_url.trim_end_matches('/')), "check whether Ollama is running")
        .config()
        .timeout_global(Some(Duration::from_millis(1200)))
        .build()
        .call()
        .is_ok()
}

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Running,
    /// Installed, but nothing is listening.
    Stopped,
    NotInstalled,
}

pub fn status(cfg: &Config) -> Status {
    if is_running(cfg) {
        Status::Running
    } else if exe().is_some() {
        Status::Stopped
    } else {
        Status::NotInstalled
    }
}

/// Make sure the server is up, starting it if it is merely stopped.
///
/// Called before a rewrite, so it must be cheap when everything is already
/// fine: the running check is a 1.2s-capped request and the common path stops
/// there.
pub fn ensure_running(cfg: &Config) -> Result<()> {
    if is_running(cfg) {
        return Ok(());
    }
    let Some(exe) = exe() else {
        bail!("Ollama is not installed. Get it from https://ollama.com/download");
    };

    crate::log!("starting Ollama…");
    let mut cmd = Command::new(exe);
    cmd.arg("serve").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // Deliberately not adopted into the child job object: this server outlives
    // Verba on purpose, since other tools on the machine may be using it.
    cmd.spawn().map_err(|e| anyhow!("could not start Ollama: {e}"))?;

    // Loading is quick but not instant; poll rather than guess a sleep.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if is_running(cfg) {
            crate::log!("Ollama is up");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    bail!("Ollama was started but did not answer within 20s")
}

/// Force the configured model resident, so the first dictation does not pay
/// its load.
///
/// Ollama loads weights on the first request, not when the server starts. A
/// large model can take well over a minute of that, which blows straight
/// through the rewrite timeout and makes the first dictation after a cold
/// start silently fall back. Run this in the background at startup; the
/// generous budget here is affordable precisely because nobody is waiting.
pub fn preload(cfg: &Config) -> Result<()> {
    ensure_running(cfg)?;
    crate::net::post(&format!("{}/api/generate", cfg.llm_url.trim_end_matches('/')), "warm up the rewrite model")
        .config()
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        // An empty prompt loads the weights without generating anything.
        // keep_alive holds them past Ollama's five-minute default, so a quiet
        // spell between dictations does not undo this.
        .send_json(serde_json::json!({
            "model": cfg.llm_model,
            "prompt": "",
            "stream": false,
            "keep_alive": "30m",
        }))
        .map_err(|e| anyhow!("preload failed: {e}"))?;
    Ok(())
}

// --- model catalogue ------------------------------------------------------

#[derive(Serialize, Clone)]
pub struct LlmModel {
    pub name: String,
    pub params: &'static str,
    /// Real download size, read from the registry manifest rather than
    /// estimated.
    pub size_gb: f32,
    pub note: &'static str,
    pub installed: bool,
    pub recommended: bool,
    /// True for a model already on the machine that is not in our list.
    pub local_only: bool,
}

/// Curated small models. Capped at 4B: this runs between the user finishing a
/// sentence and their text appearing, so a model that takes ten seconds is the
/// wrong trade however much better it writes. Sizes verified against the
/// registry manifests.
const CURATED: &[(&str, &str, f32, u64, &str)] = &[
    ("gemma3:1b", "1B", 0.76, 2_500,
     "Smallest useful option. Fine for punctuation and tidying on any machine."),
    ("llama3.2:1b", "1B", 1.23, 3_000,
     "Quick and even-handed. A good floor if Gemma reads too terse."),
    ("qwen3:1.7b", "1.7B", 1.27, 4_000,
     "Noticeably better structure than the 1B models for a little more memory."),
    ("smollm2:1.7b", "1.7B", 1.70, 4_000,
     "Trained for instruction following at small size. Strong at reformatting."),
    ("qwen2.5:3b", "3B", 1.80, 6_000,
     "Reliable all-rounder. Handles bullets and email register well."),
    ("llama3.2:3b", "3B", 1.88, 6_000,
     "Best of the 3B class for natural prose."),
    ("qwen3:4b", "4B", 2.33, 8_000,
     "The most capable rewriter under the size cap. Wants a GPU to stay snappy."),
    ("phi4-mini", "3.8B", 2.32, 8_000,
     "Sharp on structure and lists. Slightly stiff prose."),
    ("gemma3:4b", "4B", 3.11, 8_000,
     "Strong writer, largest here. Only worth it with VRAM to spare."),
];

/// One named pick per memory tier, largest budget first.
///
/// Stated rather than derived from the catalogue's ordering: several models
/// share a tier, so "the biggest that fits" silently resolved ties by array
/// position — reordering the list for display would have changed the
/// recommendation without anyone touching this logic.
const TIERS: &[(u64, &str)] = &[
    (8_000, "qwen3:4b"),      // smaller download than gemma3:4b, writes as well
    (6_000, "llama3.2:3b"),   // best of the 3B class for natural prose
    (4_000, "qwen3:1.7b"),
    (2_500, "gemma3:1b"),
];

/// Pick a model this machine can run *fast*, not merely hold.
///
/// Fitting in memory is the wrong test on its own. A 4B model decodes at a
/// handful of tokens a second on CPU, so a RAM-only machine with plenty of
/// memory would be recommended something that adds seconds to every
/// dictation. Without GPU offload the recommendation is capped at 1.7B
/// regardless of how much RAM there is.
pub fn recommended_for(hw: &crate::hardware::Hardware) -> &'static str {
    let offload = hw.gpu_backend != "cpu" && hw.vram_mb >= 2_000;
    if !offload {
        // 12GB, not 8: on an 8GB machine a 1.7B model resident alongside the
        // speech model and the OS leaves nothing for the app being dictated
        // into, and swapping costs far more than the better wording is worth.
        return if hw.ram_mb >= 12_000 { "qwen3:1.7b" } else { "gemma3:1b" };
    }
    TIERS
        .iter()
        .find(|(needs, _)| hw.vram_mb >= *needs)
        .map(|(_, name)| *name)
        // Below every tier there is still a right answer; refusing to
        // recommend would just leave the user guessing.
        .unwrap_or("gemma3:1b")
}

pub fn catalogue(cfg: &Config, hw: &crate::hardware::Hardware) -> Vec<LlmModel> {
    let local = crate::llm::installed_models(cfg);
    let pick = recommended_for(hw);

    let mut out: Vec<LlmModel> = CURATED
        .iter()
        .map(|(name, params, size_gb, _, note)| LlmModel {
            name: (*name).into(),
            params,
            size_gb: *size_gb,
            note,
            // Ollama reports "gemma3:1b"; a bare name resolves to ":latest".
            installed: local.iter().any(|l| l == name || l.trim_end_matches(":latest") == *name),
            recommended: *name == pick,
            local_only: false,
        })
        .collect();

    // Anything already on the machine that we do not list, so a user who has
    // pulled their own model can still select it.
    for l in local {
        if !out.iter().any(|m| m.name == l || l.trim_end_matches(":latest") == m.name) {
            out.push(LlmModel {
                name: l,
                params: "",
                size_gb: 0.0,
                note: "Already on this machine.",
                installed: true,
                recommended: false,
                local_only: true,
            });
        }
    }
    out
}

/// Pull a model, reporting progress as Ollama streams it.
pub fn pull<F: FnMut(&str, u64, u64)>(cfg: &Config, model: &str, mut on: F) -> Result<()> {
    ensure_running(cfg)?;

    let resp = crate::net::post(&format!("{}/api/pull", cfg.llm_url.trim_end_matches('/')), "download an Ollama model")
        .config()
        // A large pull legitimately runs for many minutes; the per-line reads
        // below are what would stall if the server died.
        .timeout_global(None)
        .build()
        .send_json(serde_json::json!({ "model": model, "stream": true }))
        .map_err(|e| anyhow!("pull failed: {e}"))?;

    #[derive(serde::Deserialize)]
    struct Line {
        #[serde(default)]
        status: String,
        #[serde(default)]
        total: u64,
        #[serde(default)]
        completed: u64,
        #[serde(default)]
        error: Option<String>,
    }

    // NDJSON: one status object per line until the stream closes.
    for line in BufReader::new(resp.into_body().into_reader()).lines() {
        let line = line.map_err(|e| anyhow!("pull stream broke: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(l) = serde_json::from_str::<Line>(&line) else { continue };
        if let Some(e) = l.error {
            bail!("{e}");
        }
        on(&l.status, l.completed, l.total);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::Hardware;

    fn hw(vram_mb: u64, ram_mb: u64) -> Hardware {
        Hardware { gpu: "test".into(), vram_mb, ram_mb, cores: 8, threads: 16, gpu_backend: "vulkan" }
    }

    fn cpu_only(ram_mb: u64) -> Hardware {
        Hardware { gpu_backend: "cpu", vram_mb: 0, ..hw(0, ram_mb) }
    }

    /// The recommendation has to fit what the machine can actually hold, or
    /// the rewrite spills to CPU and the whole point — a pass fast enough to
    /// sit between speaking and seeing the text — is lost.
    #[test]
    fn recommendation_tracks_available_memory() {
        // Discrete GPU with room to spare.
        assert_eq!(recommended_for(&hw(16_000, 32_000)), "qwen3:4b");
        // Modest laptop GPU.
        assert_eq!(recommended_for(&hw(4_000, 16_000)), "qwen3:1.7b");
        // Enough VRAM to offload, but only for the smallest.
        assert_eq!(recommended_for(&hw(2_500, 16_000)), "gemma3:1b");
    }

    /// Plenty of RAM is not a reason to recommend a 4B: without offload it
    /// decodes at a few tokens a second and stalls every dictation.
    #[test]
    fn cpu_only_is_capped_however_much_ram_there_is() {
        assert_eq!(recommended_for(&cpu_only(64_000)), "qwen3:1.7b");
        assert_eq!(recommended_for(&cpu_only(32_000)), "qwen3:1.7b");
        // 8GB is not enough to hold a 1.7B beside the speech model.
        assert_eq!(recommended_for(&cpu_only(8_000)), "gemma3:1b");
        assert_eq!(recommended_for(&cpu_only(4_000)), "gemma3:1b");
    }

    /// A GPU too small to hold anything must take the CPU path rather than
    /// being treated as offload-capable.
    #[test]
    fn a_tiny_gpu_is_not_treated_as_offload() {
        assert_eq!(recommended_for(&hw(1_000, 8_000)), "gemma3:1b");
        assert_eq!(recommended_for(&hw(1_000, 32_000)), "qwen3:1.7b");
    }

    /// A tier naming a model the catalogue does not offer would leave the UI
    /// marking nothing as recommended, with no error anywhere.
    #[test]
    fn every_tier_names_a_real_model() {
        for (_, name) in TIERS {
            assert!(
                CURATED.iter().any(|(n, ..)| n == name),
                "{name} is recommended but is not in the catalogue"
            );
        }
    }

    /// The size cap is the point of this list: the rewrite sits between the
    /// user speaking and their text appearing.
    #[test]
    fn nothing_in_the_catalogue_exceeds_the_cap() {
        for (name, _, gb, _, _) in CURATED {
            assert!(*gb <= 3.2, "{name} at {gb}GB is past the small-model cap");
        }
    }
}
