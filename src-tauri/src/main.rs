//! Verba — M1: hotkey → record → transcribe → paste.
//!
//! No UI. The overlay (M2) becomes a consumer of the same state transitions this
//! loop already makes, so nothing here changes when it lands.

mod audio;
mod focus;
mod hotkey;
mod inject;
mod stt;

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::time::Instant;

const MODEL: &str = "ggml-small.en-q5_1.bin";
/// Below this, it's a stray keypress rather than speech.
const MIN_SPEECH_MS: u128 = 300;

fn model_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("VERBA_MODEL") {
        return Ok(PathBuf::from(p));
    }
    // cargo runs from src-tauri/; a bundled build runs from alongside the exe.
    for base in ["../models", "models", "."] {
        let p = PathBuf::from(base).join(MODEL);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(anyhow!(
        "{MODEL} not found. Put it in models/, or set VERBA_MODEL."
    ))
}

fn main() -> Result<()> {
    // Windows consoles default to a legacy codepage, which mangles any non-ASCII
    // in the transcript — curly apostrophes, accents, dashes.
    unsafe {
        let _ = windows::Win32::System::Console::SetConsoleOutputCP(65001);
    }

    let engine = stt::Engine::new(&model_path()?)?;
    let recorder = audio::Recorder::new()?;
    let events = hotkey::spawn()?;

    println!("\nready — hold Ctrl+Shift+Space to dictate, Ctrl+C to quit\n");

    let mut mark = 0u64;
    let mut started = Instant::now();
    let mut app = focus::App::default();

    for ev in events {
        match ev {
            hotkey::Event::Pressed => {
                mark = recorder.mark();
                started = Instant::now();
                app = focus::foreground();
                println!("● listening   [{}]", app.exe);
            }

            hotkey::Event::Released => {
                let held = started.elapsed();
                let raw = recorder.take_since(mark);

                if held.as_millis() < MIN_SPEECH_MS || raw.is_empty() {
                    println!("  too short, ignored\n");
                    continue;
                }

                let t0 = Instant::now();
                let pcm = audio::to_16k(&raw, recorder.sample_rate())?;
                let resampled = t0.elapsed();

                let t1 = Instant::now();
                let text = engine.transcribe(&pcm)?;
                let transcribed = t1.elapsed();

                if text.is_empty() {
                    println!("  (silence)\n");
                    continue;
                }

                println!("  {text}");
                println!(
                    "  {:.1}s audio · resample {}ms · whisper {}ms · {:.1}x realtime",
                    held.as_secs_f32(),
                    resampled.as_millis(),
                    transcribed.as_millis(),
                    held.as_secs_f32() / transcribed.as_secs_f32().max(0.001),
                );
                if !app.title.is_empty() {
                    println!("  → {} · {}", app.exe, app.title);
                }

                inject::insert(&text)?;
                println!();
            }
        }
    }
    Ok(())
}
