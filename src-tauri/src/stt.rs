//! whisper.cpp transcription.

use anyhow::{anyhow, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct Engine {
    ctx: WhisperContext,
    threads: i32,
}

/// Leave headroom so dictating doesn't make the rest of the machine stutter.
///
/// This is the whole point of the app on a laptop: a transcription that takes
/// 800ms while music keeps playing beats one that takes 500ms and drops frames.
/// Physical cores, not logical — whisper's GEMM kernels gain nothing from SMT
/// and contend with everything else when oversubscribed.
fn default_threads() -> i32 {
    num_cpus::get_physical().saturating_sub(2).max(1) as i32
}

impl Engine {
    pub fn new(model: &Path) -> Result<Self> {
        if !model.exists() {
            return Err(anyhow!("model not found: {}", model.display()));
        }
        let path = model.to_str().ok_or_else(|| anyhow!("non-UTF8 model path"))?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())?;
        let threads = default_threads();
        println!("model: {} ({} threads)", model.display(), threads);
        Ok(Self { ctx, threads })
    }

    pub fn transcribe(&self, pcm16k: &[f32]) -> Result<String> {
        let mut state = self.ctx.create_state()?;

        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_n_threads(self.threads);
        p.set_language(Some("en"));
        p.set_translate(false);
        p.set_suppress_blank(true);
        // whisper.cpp prints to stdout by default; we want our own output only.
        p.set_print_special(false);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_timestamps(false);

        state.full(p, pcm16k)?;

        let mut out = String::new();
        for seg in state.as_iter() {
            // Lossy: whisper can emit a partial multi-byte token at a segment
            // boundary, and dropping the whole utterance over one bad char is worse.
            out.push_str(&seg.to_str_lossy()?);
        }
        Ok(out.trim().to_string())
    }
}
