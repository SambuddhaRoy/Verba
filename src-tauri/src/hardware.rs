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
    pub model: &'static str,
    pub reason: String,
}

/// Pick sensible defaults for this machine.
pub fn recommend(hw: &Hardware) -> Recommendation {
    // whisper.cpp in every case. faster-whisper needs NVIDIA CUDA specifically
    // — no Vulkan, no Intel iGPU — and a Python runtime, so it only makes sense
    // as an explicit opt-in on an NVIDIA box.
    let gpu_usable = hw.gpu_backend != "cpu" && hw.vram_mb >= 2_000;

    let (model, why) = if gpu_usable && hw.vram_mb >= 6_000 {
        (
            "ggml-large-v3-turbo-q5_0.bin",
            format!("{} has {} GB of VRAM, enough for the largest model with room to spare",
                    hw.gpu, hw.vram_mb / 1024),
        )
    } else if gpu_usable || hw.ram_mb >= 16_000 {
        (
            "ggml-small.en-q5_1.bin",
            if gpu_usable {
                format!("{} can run Small comfortably", hw.gpu)
            } else {
                format!("{} GB of RAM, no usable GPU backend — Small is the accuracy sweet spot on CPU",
                        hw.ram_mb / 1024)
            },
        )
    } else {
        (
            "ggml-base.en-q5_1.bin",
            format!("{} GB of RAM and no GPU offload — Base keeps dictation responsive",
                    hw.ram_mb.max(1) / 1024),
        )
    };

    Recommendation { engine: "whisper.cpp", model, reason: why }
}
