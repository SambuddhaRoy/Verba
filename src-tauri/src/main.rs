//! Verba — local-first speech to text.
//!
//! The engine runs on its own thread and emits state; the overlay is a pure
//! consumer of that state and holds no logic of its own.

// GUI subsystem: no console window when launched from Explorer. log::init()
// attaches to the parent terminal when there is one, so CLI use still works.
#![windows_subsystem = "windows"]

#[macro_use]
mod log;

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
const EVENT: &str = "verba:state";
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

/// Emit and report failure. Swallowing this is what hid the ACL denial that
/// kept the overlay stuck at idle.
fn emit(app: &AppHandle, state: State) {
    if let Err(e) = app.emit(EVENT, state) {
        log!("  emit failed: {e}");
    }
}

fn model_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("VERBA_MODEL") {
        return Ok(PathBuf::from(p));
    }
    let mut roots = Vec::new();
    // Next to the .exe first: double-clicked from Explorer, the working
    // directory is not necessarily the install folder.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.join("models"));
            roots.push(dir.to_path_buf());
        }
    }
    roots.push(PathBuf::from("../models"));
    roots.push(PathBuf::from("models"));

    for root in &roots {
        let p = root.join(MODEL);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(anyhow!(
        "{MODEL} not found. Looked in: {}. Set VERBA_MODEL to override.",
        roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
    ))
}

fn engine_loop(app: AppHandle) -> Result<()> {
    let engine = stt::Engine::new(&model_path()?)?;
    let recorder = audio::Recorder::new()?;
    let events = hotkey::spawn()?;

    let overlay = app
        .get_webview_window("overlay")
        .ok_or_else(|| anyhow!("overlay window missing"))?;

    log!("ready — hold Ctrl+Shift+Space to dictate");

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
                let info = focus::foreground();
                log!("● listening   [{}] {}", info.exe, info.title);

                let _ = overlay.show();
                emit(&app, State::new("listening", "LISTENING"));
            }

            Ok(hotkey::Event::Released) => {
                listening = false;
                let held = started.elapsed();
                let raw = recorder.take_since(mark);

                if held.as_millis() < MIN_SPEECH_MS || raw.is_empty() {
                    log!("  too short, ignored");
                    emit(&app, State::new("idle", ""));
                    let _ = overlay.hide();
                    continue;
                }

                emit(&app, State::new("transcribing", "TRANSCRIBING"));

                let pcm = audio::to_16k(&raw, recorder.sample_rate())?;
                let t0 = Instant::now();
                let text = engine.transcribe(&pcm)?;
                let took = t0.elapsed();

                if text.is_empty() {
                    log!("  (silence)");
                    emit(&app, State::new("idle", ""));
                    let _ = overlay.hide();
                    continue;
                }

                log!("  {text}");
                log!(
                    "  {:.1}s audio · whisper {}ms · {:.1}x realtime",
                    held.as_secs_f32(),
                    took.as_millis(),
                    held.as_secs_f32() / took.as_secs_f32().max(0.001)
                );

                let mut done = State::new("transcribing", "INSERTED");
                done.text = Some(text.clone());
                emit(&app, done);

                if let Err(e) = inject::insert(&text) {
                    log!("  insert failed: {e}");
                }

                hide_at = Some(Instant::now() + LINGER);
            }

            Err(RecvTimeoutError::Timeout) => {
                if listening {
                    let mut s = State::new("listening", "LISTENING");
                    s.level = recorder.recent_level(120);
                    s.elapsed = started.elapsed().as_secs_f32();
                    emit(&app, s);
                } else if hide_at.is_some_and(|t| Instant::now() >= t) {
                    hide_at = None;
                    emit(&app, State::new("idle", ""));
                    let _ = overlay.hide();
                }
            }

            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

/// Drive the overlay through its states with no microphone and no model, so the
/// visuals can be checked on their own.
fn overlay_demo(app: AppHandle) {
    let Some(win) = app.get_webview_window("overlay") else { return };
    let _ = win.show();
    log!("overlay demo — listening 6s, then transcript 6s");

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(6) {
        let mut s = State::new("listening", "LISTENING");
        // Fake a speaking rhythm so the ribbons visibly breathe.
        let t = start.elapsed().as_secs_f32();
        s.level = 0.45 + 0.55 * (t * 2.7).sin().abs();
        s.elapsed = t;
        emit(&app, s);
        std::thread::sleep(TICK);
    }

    let mut done = State::new("transcribing", "INSERTED");
    done.text = Some(
        "Thanks for sending the deck over — I read the pricing section this \
         morning and it mostly holds up."
            .into(),
    );
    emit(&app, done);
    std::thread::sleep(Duration::from_secs(6));

    emit(&app, State::new("idle", ""));
    let _ = win.hide();
    app.exit(0);
}

fn main() -> Result<()> {
    log::init();
    log!("verba — log at {}", log::path().display());

    let args: Vec<String> = std::env::args().collect();
    let arg1 = args.get(1).map(String::as_str);

    // `verba --inject-test "some text"` — exercises the injection path alone,
    // with no model load and no microphone. Focus a text box during the count.
    if arg1 == Some("--inject-test") {
        let text = args.get(2).map(String::as_str).unwrap_or("Verba injection test.");
        for n in (1..=4).rev() {
            log!("focus a text box… {n}");
            std::thread::sleep(Duration::from_secs(1));
        }
        inject::insert(text)?;
        log!("injected {} chars", text.chars().count());
        return Ok(());
    }

    let demo = arg1 == Some("--overlay-test");

    tauri::Builder::default()
        .setup(move |app| {
            let overlay_win = app
                .get_webview_window("overlay")
                .ok_or("overlay window missing")?;
            overlay::configure(&overlay_win)?;

            let handle = app.handle().clone();
            // The audio stream is !Send on Windows, so the engine owns everything
            // on one thread rather than sharing it.
            std::thread::Builder::new().name("engine".into()).spawn(move || {
                if demo {
                    overlay_demo(handle);
                } else if let Err(e) = engine_loop(handle) {
                    log!("engine stopped: {e:#}");
                }
            })?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|e| anyhow!("tauri: {e}"))
}
