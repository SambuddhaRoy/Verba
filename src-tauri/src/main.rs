//! Verba — local-first speech to text.
//!
//! The engine runs on its own thread and emits state; the overlay is a pure
//! consumer of that state and holds no logic of its own.

// Console stays attached in release for now: the transcript and timing logs are
// how this gets verified. Flip to `windows_subsystem = "windows"` once the tray
// and history window make it redundant.

mod audio;
mod focus;
mod hotkey;
mod inject;
mod overlay;
mod stt;

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const MODEL: &str = "ggml-small.en-q5_1.bin";
const EVENT: &str = "verba://state";
/// Below this, it's a stray keypress rather than speech.
const MIN_SPEECH_MS: u128 = 300;
/// How long the transcript stays up after insertion, to read back what landed.
const LINGER: Duration = Duration::from_millis(2600);
/// Level updates while listening. ~30fps is plenty for a wave this soft.
const TICK: Duration = Duration::from_millis(33);

#[derive(Clone, serde::Serialize)]
struct State {
    phase: &'static str,
    status: &'static str,
    level: f32,
    elapsed: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

impl State {
    fn new(phase: &'static str, status: &'static str) -> Self {
        Self { phase, status, level: 0.0, elapsed: 0.0, text: None }
    }
}

fn model_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("VERBA_MODEL") {
        return Ok(PathBuf::from(p));
    }
    for base in ["../models", "models", "."] {
        let p = PathBuf::from(base).join(MODEL);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(anyhow!("{MODEL} not found. Put it in models/, or set VERBA_MODEL."))
}

fn engine_loop(app: AppHandle) -> Result<()> {
    let engine = stt::Engine::new(&model_path()?)?;
    let recorder = audio::Recorder::new()?;
    let events = hotkey::spawn()?;

    let overlay = app
        .get_webview_window("overlay")
        .ok_or_else(|| anyhow!("overlay window missing"))?;

    println!("\nready — hold Ctrl+Shift+Space to dictate\n");

    let mut listening = false;
    let mut mark = 0u64;
    let mut started = Instant::now();
    let mut hide_at: Option<Instant> = None;

    loop {
        match events.recv_timeout(TICK) {
            Ok(hotkey::Event::Pressed) => {
                mark = recorder.mark();
                started = Instant::now();
                listening = true;
                hide_at = None;

                // Both fields get logged: writing app-routing rules in M3 means
                // knowing what the exe and title actually look like.
                let app_info = focus::foreground();
                println!("● listening   [{}] {}", app_info.exe, app_info.title);

                let _ = overlay.show();
                let _ = app.emit(EVENT, State::new("listening", "LISTENING"));
            }

            Ok(hotkey::Event::Released) => {
                listening = false;
                let held = started.elapsed();
                let raw = recorder.take_since(mark);

                if held.as_millis() < MIN_SPEECH_MS || raw.is_empty() {
                    println!("  too short, ignored\n");
                    let _ = app.emit(EVENT, State::new("idle", ""));
                    let _ = overlay.hide();
                    continue;
                }

                let _ = app.emit(EVENT, State::new("transcribing", "TRANSCRIBING"));

                let pcm = audio::to_16k(&raw, recorder.sample_rate())?;
                let t0 = Instant::now();
                let text = engine.transcribe(&pcm)?;
                let took = t0.elapsed();

                if text.is_empty() {
                    println!("  (silence)\n");
                    let _ = app.emit(EVENT, State::new("idle", ""));
                    let _ = overlay.hide();
                    continue;
                }

                println!("  {text}");
                println!(
                    "  {:.1}s audio · whisper {}ms · {:.1}x realtime",
                    held.as_secs_f32(),
                    took.as_millis(),
                    held.as_secs_f32() / took.as_secs_f32().max(0.001),
                );

                let mut done = State::new("transcribing", "INSERTED");
                done.text = Some(text.clone());
                let _ = app.emit(EVENT, done);

                if let Err(e) = inject::insert(&text) {
                    eprintln!("  insert failed: {e}");
                }
                println!();

                hide_at = Some(Instant::now() + LINGER);
            }

            Err(RecvTimeoutError::Timeout) => {
                if listening {
                    let mut s = State::new("listening", "LISTENING");
                    s.level = recorder.recent_level(120);
                    s.elapsed = started.elapsed().as_secs_f32();
                    let _ = app.emit(EVENT, s);
                } else if hide_at.is_some_and(|t| Instant::now() >= t) {
                    hide_at = None;
                    let _ = app.emit(EVENT, State::new("idle", ""));
                    let _ = overlay.hide();
                }
            }

            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    unsafe {
        let _ = windows::Win32::System::Console::SetConsoleOutputCP(65001);
    }

    // `verba --inject-test "some text"` — exercises the injection path alone,
    // with no model load and no microphone. Focus a text box during the count.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--inject-test") {
        let text = args.get(2).map(String::as_str).unwrap_or("Verba injection test.");
        for n in (1..=4).rev() {
            println!("focus a text box… {n}");
            std::thread::sleep(Duration::from_secs(1));
        }
        inject::insert(text)?;
        println!("injected {} chars", text.chars().count());
        return Ok(());
    }

    tauri::Builder::default()
        .setup(|app| {
            let overlay_win = app
                .get_webview_window("overlay")
                .ok_or("overlay window missing")?;
            overlay::configure(&overlay_win)?;

            let handle = app.handle().clone();
            // The audio stream is !Send on Windows, so the engine owns everything
            // on one thread rather than sharing it.
            std::thread::Builder::new().name("engine".into()).spawn(move || {
                if let Err(e) = engine_loop(handle) {
                    eprintln!("engine stopped: {e:#}");
                }
            })?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|e| anyhow!("tauri: {e}"))
}
