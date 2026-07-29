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
mod config;
mod focus;
mod hardware;
mod hotkey;
mod inject;
mod overlay;
mod startup;
mod stt;

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

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
    elapsed: f32,
    bands: [f32; audio::BANDS],
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    visual: Option<String>,
}

impl State {
    fn new(phase: &'static str, status: &'static str) -> Self {
        Self {
            phase,
            status,
            elapsed: 0.0,
            bands: [0.0; audio::BANDS],
            text: None,
            visual: None,
        }
    }
}

/// Emit and report failure. Swallowing this is what hid the ACL denial that
/// once kept the overlay stuck at idle.
fn emit(app: &AppHandle, state: State) {
    if let Err(e) = app.emit(EVENT, state) {
        log!("  emit failed: {e}");
    }
}

fn model_path(file: &str) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("VERBA_MODEL") {
        return Ok(PathBuf::from(p));
    }
    let p = config::models_dir().join(file);
    if p.exists() {
        return Ok(p);
    }
    Err(anyhow!(
        "{file} not found in {}. Pick another model in Settings, or set VERBA_MODEL.",
        config::models_dir().display()
    ))
}

// --- settings commands ----------------------------------------------------

#[derive(serde::Serialize)]
struct SettingsState {
    config: config::Config,
    hardware: hardware::Hardware,
    recommendation: hardware::Recommendation,
    models: Vec<config::ModelInfo>,
    microphones: Vec<String>,
    engines: Vec<&'static str>,
    log_path: String,
    config_path: String,
}

#[tauri::command]
fn get_state() -> SettingsState {
    let hw = hardware::detect();
    SettingsState {
        config: config::load(),
        recommendation: hardware::recommend(&hw),
        hardware: hw,
        models: config::catalogue(),
        microphones: audio::input_devices(),
        // faster-whisper is not wired up yet; the UI greys it out rather than
        // offering a selection that would silently do nothing.
        engines: vec!["whisper.cpp"],
        log_path: log::path().display().to_string(),
        config_path: config::path().display().to_string(),
    }
}

#[tauri::command]
fn set_config(app: AppHandle, cfg: config::Config) -> Result<(), String> {
    let previous = config::load();
    config::save(&cfg).map_err(|e| e.to_string())?;

    if cfg.launch_at_startup != previous.launch_at_startup {
        if let Err(e) = startup::set(cfg.launch_at_startup) {
            return Err(format!("saved, but startup entry failed: {e}"));
        }
    }
    // Push the overlay treatment through immediately — it is the one setting
    // with an instant visible effect, so waiting for the next dictation to
    // apply it would read as the control being broken.
    if cfg.visual != previous.visual {
        let mut s = State::new("idle", "");
        s.visual = Some(cfg.visual.clone());
        emit(&app, s);
    }
    Ok(())
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// --- engine ---------------------------------------------------------------

fn engine_loop(app: AppHandle) -> Result<()> {
    let mut cfg = config::load();
    // Held as an Option so it can be dropped and rebuilt: that is what makes
    // "preload" and "unload when idle" real controls rather than stored values
    // nothing ever reads.
    let mut engine: Option<stt::Engine> = None;
    let mut loaded_model = String::new();
    let mut last_used = Instant::now();

    if cfg.preload_model {
        engine = Some(stt::Engine::new(&model_path(&cfg.model)?, cfg.threads)?);
        loaded_model = cfg.model.clone();
    }

    let recorder = audio::Recorder::new(cfg.microphone.as_deref())?;
    let events = hotkey::spawn()?;

    let overlay = app
        .get_webview_window("overlay")
        .ok_or_else(|| anyhow!("overlay window missing"))?;

    // Send the configured treatment once, so the overlay starts in the right
    // one rather than defaulting to ribbons until the first settings change.
    let mut boot = State::new("idle", "");
    boot.visual = Some(cfg.visual.clone());
    emit(&app, boot);

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

                // Both fields get logged: writing app-routing rules later means
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

                // Re-read settings each time so a model change in the settings
                // window takes effect on the next dictation, not the next launch.
                cfg = config::load();
                if engine.is_none() || loaded_model != cfg.model {
                    let path = model_path(&cfg.model)?;
                    log!("  loading {}", cfg.model);
                    engine = Some(stt::Engine::new(&path, cfg.threads)?);
                    loaded_model = cfg.model.clone();
                }
                let engine_ref = engine.as_ref().expect("just loaded");

                let pcm = audio::to_16k(&raw, recorder.sample_rate())?;
                let t0 = Instant::now();
                let text = engine_ref.transcribe(&pcm)?;
                let took = t0.elapsed();
                last_used = Instant::now();

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
                    s.bands = recorder.bands();
                    s.elapsed = started.elapsed().as_secs_f32();
                    emit(&app, s);
                } else if hide_at.is_some_and(|t| Instant::now() >= t) {
                    hide_at = None;
                    emit(&app, State::new("idle", ""));
                    let _ = overlay.hide();
                } else if engine.is_some()
                    && cfg.model_idle_eject_secs > 0
                    && last_used.elapsed().as_secs() >= cfg.model_idle_eject_secs
                {
                    // Dropping the context frees the model's memory; the next
                    // dictation pays the reload.
                    engine = None;
                    loaded_model.clear();
                    log!("model unloaded after {}s idle", cfg.model_idle_eject_secs);
                }
            }

            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

/// Drive the overlay through its states with no microphone and no model, so the
/// visuals can be checked on their own.
fn overlay_demo(app: AppHandle, visual: &str) {
    let Some(win) = app.get_webview_window("overlay") else { return };
    let _ = win.show();
    log!("overlay demo ({visual}) — listening 8s, then transcript 6s");

    let mut s = State::new("idle", "");
    s.visual = Some(visual.to_string());
    emit(&app, s);
    std::thread::sleep(Duration::from_millis(120));

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        let t = start.elapsed().as_secs_f32();
        let mut s = State::new("listening", "LISTENING");
        // Bands sweep at different rates so the treatment is visibly
        // frequency-reactive rather than one level moving everything together.
        for (i, b) in s.bands.iter_mut().enumerate() {
            let f = 1.3 + i as f32 * 0.55;
            *b = (0.5 + 0.5 * (t * f + i as f32).sin()).powf(1.6);
        }
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

    let demo = (arg1 == Some("--overlay-test"))
        .then(|| args.get(2).cloned().unwrap_or_else(|| config::load().visual));
    let open_settings = arg1 == Some("--settings");

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_state, set_config])
        .setup(move |app| {
            let overlay_win = app
                .get_webview_window("overlay")
                .ok_or("overlay window missing")?;
            overlay::configure(&overlay_win)?;

            let settings_item = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Verba", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&settings_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Verba — hold Ctrl+Shift+Space to dictate")
                .menu(&menu)
                // Left click opens Settings; the menu stays on right click.
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "settings" => show_settings(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                        show_settings(tray.app_handle());
                    }
                })
                .build(app)?;

            if open_settings {
                show_settings(app.handle());
            }

            let handle = app.handle().clone();
            // The audio stream is !Send on Windows, so the engine owns everything
            // on one thread rather than sharing it.
            std::thread::Builder::new().name("engine".into()).spawn(move || {
                match &demo {
                    Some(v) => overlay_demo(handle, v),
                    None => {
                        if let Err(e) = engine_loop(handle) {
                            log!("engine stopped: {e:#}");
                        }
                    }
                }
            })?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|e| anyhow!("tauri: {e}"))
}
