//! Hardware probe, used to recommend an engine and a model.
//!
//! DXGI and GlobalMemoryStatusEx rather than a crate: the `windows` dependency
//! is already here, and this is perhaps forty lines.

use serde::Serialize;

use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG,
    DXGI_ADAPTER_FLAG_SOFTWARE,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

#[derive(Serialize, Clone, Debug)]
pub struct Hardware {
    pub gpu: String,
    pub vram_mb: u64,
    pub ram_mb: u64,
    pub cores: usize,
    pub threads: usize,
    /// Whether this build can actually reach the GPU.
    pub gpu_backend: &'static str,
}

/// Compiled-in backend. Reporting the build's capability, not the machine's:
/// a Vulkan-capable GPU is useless if the binary has no Vulkan backend.
const GPU_BACKEND: &str = if cfg!(feature = "gpu-vulkan") { "vulkan" } else { "cpu" };

fn largest_gpu() -> (String, u64) {
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return ("unknown".into(), 0);
        };
        let mut best = (String::from("none"), 0u64);
        for i in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(i) else { break };
            let Ok(desc) = adapter.GetDesc1() else { continue };
            // Skip the Basic Render Driver; it reports memory it cannot use.
            if DXGI_ADAPTER_FLAG(desc.Flags as i32) == DXGI_ADAPTER_FLAG_SOFTWARE {
                continue;
            }
            let vram = (desc.DedicatedVideoMemory / 1024 / 1024) as u64;
            if vram > best.1 {
                let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
                best = (String::from_utf16_lossy(&desc.Description[..end]), vram);
            }
            let _: IDXGIAdapter1 = adapter;
        }
        best
    }
}

fn ram_mb() -> u64 {
    unsafe {
        let mut s = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut s).is_ok() {
            s.ullTotalPhys / 1024 / 1024
        } else {
            0
        }
    }
}

pub fn detect() -> Hardware {
    let (gpu, vram_mb) = largest_gpu();
    Hardware {
        gpu,
        vram_mb,
        ram_mb: ram_mb(),
        cores: num_cpus::get_physical(),
        threads: num_cpus::get(),
        gpu_backend: GPU_BACKEND,
    }
}

#[derive(Serialize, Clone)]
pub struct Recommendation {
    pub engine: &'static str,
    /// Owned rather than `&'static str`: it comes straight off the scored
    /// catalogue, and keeping a second hardcoded list of file names in sync
    /// just to borrow one is how the two drift apart.
    pub model: String,
    pub reason: String,
}

/// Whether a model of this footprint can be offloaded to the GPU.
fn offloads(hw: &Hardware, needs_mb: u32) -> bool {
    hw.gpu_backend != "cpu" && hw.vram_mb >= 2_000 && hw.vram_mb >= needs_mb as u64
}

/// Whether this machine can run the model at all. RAM is the backstop: without
/// GPU offload the weights live in system memory, and a model that does not fit
/// there does not run.
pub fn fits(hw: &Hardware, needs_mb: u32) -> bool {
    offloads(hw, needs_mb) || hw.ram_mb >= needs_mb as u64
}

/// Re-rate a model's speed for this machine.
///
/// The catalogue's number assumes hardware that can hold the model. Falling
/// back to CPU costs roughly in proportion to how big the thing is — a Tiny
/// model is brisk on any CPU, Large v3 is not — so the penalty scales with
/// footprint rather than being a flat discount.
pub fn speed_on(hw: &Hardware, needs_mb: u32, speed: u8) -> u8 {
    if offloads(hw, needs_mb) {
        return speed;
    }
    if !fits(hw, needs_mb) {
        return 0;
    }
    // 1.0 for something that fits in a few hundred MB, falling to about 0.35
    // for the multi-gigabyte builds.
    let penalty = match needs_mb {
        0..=800 => 0.92,
        801..=1_500 => 0.78,
        1_501..=3_000 => 0.58,
        3_001..=4_500 => 0.44,
        _ => 0.33,
    };
    (speed as f32 * penalty).round().clamp(1.0, 100.0) as u8
}

/// How good a fit a model is for this machine, balancing the two things the
/// user actually feels: how often it is right, and how long they wait.
///
/// Accuracy is weighted a little higher than speed because every engine here is
/// already faster than realtime on hardware that suits it — the difference
/// between 80 and 95 accuracy is visible in the text, while the difference
/// between two fast models is not. A model that does not fit scores zero.
pub fn score(hw: &Hardware, needs_mb: u32, accuracy: u8, speed: u8) -> u32 {
    if !fits(hw, needs_mb) {
        return 0;
    }
    let here = speed_on(hw, needs_mb, speed) as u32;
    (accuracy as u32 * 3 + here * 2) / 5
}

/// Pick sensible defaults for this machine.
pub fn recommend(hw: &Hardware) -> Recommendation {
    // whisper.cpp in every case. faster-whisper needs NVIDIA CUDA specifically
    // — no Vulkan, no Intel iGPU — and a Python runtime, so it only makes sense
    // as an explicit opt-in on an NVIDIA box.
    let gpu_usable = hw.gpu_backend != "cpu" && hw.vram_mb >= 2_000;

    // Score every model that ships working out of the box and take the best.
    // The previous version was a hand-written VRAM ladder, which meant the
    // recommendation and the accuracy/speed bars the user compares models by
    // were two separate opinions that could disagree. One ranking now feeds
    // both, so the badge always lands on the row that scores highest.
    let best = crate::config::catalogue()
        .into_iter()
        .filter(|m| m.engine == "whisper.cpp")
        .map(|m| {
            let s = score(hw, m.needs_mb, m.accuracy, m.speed);
            (s, m)
        })
        // Ties break towards the smaller download, which is the one that costs
        // the user less to try.
        .max_by_key(|(s, m)| (*s, u32::MAX - m.size_mb))
        .map(|(_, m)| m);

    let Some(m) = best else {
        // The catalogue is a compile-time constant, so this cannot happen; if
        // it somehow does, name a model that exists rather than panicking.
        return Recommendation {
            engine: "whisper.cpp",
            model: "ggml-base.en-q5_1.bin".into(),
            reason: "no model catalogue available".into(),
        };
    };

    let why = if offloads(hw, m.needs_mb) {
        format!(
            "{} has {} GB of VRAM, so {} runs on the GPU — the best accuracy here without a wait",
            hw.gpu,
            hw.vram_mb / 1024,
            m.name
        )
    } else if gpu_usable {
        format!(
            "{} would have to spill to CPU for anything larger, so {} is the best balance",
            hw.gpu, m.name
        )
    } else {
        format!(
            "{} GB of RAM and no GPU offload — {} is the accuracy sweet spot that stays responsive",
            hw.ram_mb.max(1) / 1024,
            m.name
        )
    };

    Recommendation { engine: "whisper.cpp", model: m.file, reason: why }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(vram_mb: u64, ram_mb: u64, gpu: bool) -> Hardware {
        Hardware {
            gpu: if gpu { "Test GPU".into() } else { "none".into() },
            vram_mb,
            ram_mb,
            cores: 8,
            threads: 16,
            gpu_backend: if gpu { "vulkan" } else { "cpu" },
        }
    }

    /// The two numbers the bars are drawn from. A transposed pair would look
    /// perfectly plausible in the UI while recommending the wrong model to
    /// everyone, so the ordering that makes them meaningful is pinned here.
    #[test]
    fn accuracy_and_speed_are_the_right_way_round() {
        let all = crate::config::catalogue();
        let get = |n: &str| all.iter().find(|m| m.name == n).unwrap_or_else(|| panic!("{n} missing"));

        let tiny = get("Whisper Tiny (English)");
        let small = get("Whisper Small (English)");
        let large = get("Whisper Large v3");

        assert!(tiny.speed > small.speed && small.speed > large.speed,
                "smaller models must rate faster");
        assert!(large.accuracy > small.accuracy && small.accuracy > tiny.accuracy,
                "larger models must rate more accurate");

        // Turbo exists precisely because it is much faster than Large for
        // nearly the same accuracy. If that stops being true here, the whole
        // recommendation is off.
        let turbo = get("Whisper Large v3 Turbo");
        assert!(turbo.speed > large.speed);
        assert!(turbo.accuracy + 5 >= large.accuracy);

        for m in &all {
            assert!(m.accuracy > 0 && m.accuracy <= 100, "{}: accuracy out of range", m.name);
            assert!(m.speed > 0 && m.speed <= 100, "{}: speed out of range", m.name);
        }
    }

    /// A model too big for the machine must never be offered, and must not be
    /// silently rated as merely slow.
    #[test]
    fn models_that_do_not_fit_score_zero() {
        let weak = rig(0, 2_000, false);
        assert!(!fits(&weak, 4_600));
        assert_eq!(speed_on(&weak, 4_600, 30), 0);
        assert_eq!(score(&weak, 4_600, 96, 30), 0);
    }

    /// The same model is quick on a GPU and slow spilling to CPU. One rating
    /// for both would tell a laptop user a 547 MB model is as fast for them as
    /// it is on a workstation.
    #[test]
    fn cpu_fallback_costs_more_for_bigger_models() {
        let gpu = rig(8_000, 32_000, true);
        let cpu = rig(0, 32_000, false);

        assert_eq!(speed_on(&gpu, 3_200, 68), 68, "offloaded runs at its own rating");
        assert!(speed_on(&cpu, 3_200, 68) < 68, "CPU must cost something");
        // A small model barely notices; a large one does.
        let small_loss = 80 - speed_on(&cpu, 1_100, 80) as i32;
        let large_loss = 68 - speed_on(&cpu, 3_200, 68) as i32;
        assert!(large_loss > small_loss, "the penalty must scale with size");
    }

    /// The recommendation has to follow the hardware, and has to name a model
    /// that is actually in the catalogue and actually fits.
    #[test]
    fn recommendation_tracks_hardware() {
        let all = crate::config::catalogue();
        for (label, hw) in [
            ("workstation", rig(16_000, 64_000, true)),
            ("laptop dGPU", rig(4_000, 16_000, true)),
            ("integrated", rig(0, 16_000, false)),
            ("thin", rig(0, 4_000, false)),
        ] {
            let r = recommend(&hw);
            let m = all
                .iter()
                .find(|m| m.file == r.model)
                .unwrap_or_else(|| panic!("{label}: recommended {} is not in the catalogue", r.model));
            assert_eq!(m.engine, "whisper.cpp", "{label}: must work without a sidecar");
            assert!(fits(&hw, m.needs_mb), "{label}: recommended a model that does not fit");
            assert!(!r.reason.is_empty(), "{label}: no reason given");
        }

        // More capable hardware must never be given a worse model than weaker
        // hardware — the ladder has to be monotonic.
        let big = recommend(&rig(16_000, 64_000, true));
        let small = recommend(&rig(0, 4_000, false));
        let acc = |file: &str| all.iter().find(|m| m.file == file).unwrap().accuracy;
        assert!(acc(&big.model) >= acc(&small.model),
                "a workstation must not be recommended a less accurate model than a thin laptop");
    }
}
