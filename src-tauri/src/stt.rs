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
    /// `threads` of None means physical cores - 2.
    pub fn new(model: &Path, threads: Option<i32>) -> Result<Self> {
        if !model.exists() {
            return Err(anyhow!("model not found: {}", model.display()));
        }
        let path = model.to_str().ok_or_else(|| anyhow!("non-UTF8 model path"))?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())?;
        let threads = threads.filter(|t| *t > 0).unwrap_or_else(default_threads);
        crate::log!("model: {} ({} threads)", model.display(), threads);
        Ok(Self { ctx, threads })
    }

    /// `quick` trades a little accuracy for latency, for interim passes that
    /// will be replaced by the final one anyway.
    pub fn transcribe(&self, pcm16k: &[f32], quick: bool) -> Result<String> {
        let mut state = self.ctx.create_state()?;

        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_n_threads(self.threads);
        p.set_language(Some("en"));
        p.set_translate(false);
        p.set_suppress_blank(true);
        if quick {
            // Each interim pass re-reads the whole tail, so carrying decoder
            // context between passes would compound earlier mistakes rather
            // than correct them.
            p.set_no_context(true);
            p.set_single_segment(true);
        }
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
