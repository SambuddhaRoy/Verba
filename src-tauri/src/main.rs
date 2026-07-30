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
mod download;
mod focus;
mod hardware;
mod hotkey;
mod inject;
mod overlay;
mod startup;
mod stt;
mod transcribe;

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
/// How often to run an interim pass while the key is held.
const PARTIAL_EVERY: Duration = Duration::from_millis(850);
/// Whisper needs roughly this much audio before it produces anything useful.
const PARTIAL_MIN_SECS: f32 = 1.2;

#[derive(Clone, serde::Serialize)]
struct State {
    phase: &'static str,
    status: &'static str,
    elapsed: f32,
    bands: [f32; audio::BANDS],
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// True while `text` is an interim guess that will be replaced.
    partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    visual: Option<String>,
    /// Shown in the overlay's meta row. Sent on state changes, not every tick.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    gpu: bool,
}

impl State {
    fn new(phase: &'static str, status: &'static str) -> Self {
        Self {
            phase,
            status,
            elapsed: 0.0,
            bands: [0.0; audio::BANDS],
            text: None,
            partial: false,
            visual: None,
            model: None,
            gpu: cfg!(feature = "gpu-vulkan"),
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

pub(crate) fn model_path(file: &str) -> Result<PathBuf> {
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

// --- commands -------------------------------------------------------------

#[derive(serde::Serialize)]
struct SettingsState {
    config: config::Config,
    hardware: hardware::Hardware,
    recommendation: hardware::Recommendation,
    models: Vec<config::ModelInfo>,
    microphones: Vec<String>,
    engines: Vec<config::EngineInfo>,
    log_path: String,
    config_path: String,
    models_dir: String,
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
        // Every known engine is listed with an availability flag. Unbuilt ones
        // are shown greyed rather than hidden, so the roadmap is visible
        // without offering a choice that would silently do nothing.
        engines: config::engines(),
        log_path: log::path().display().to_string(),
        config_path: config::path().display().to_string(),
        models_dir: config::models_dir().display().to_string(),
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
    if cfg.hotkey != previous.hotkey {
        hotkey::set_binding(cfg.hotkey.vk, cfg.hotkey.mods());
        log!("hotkey rebound to {}", hotkey_label(&cfg.hotkey));
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

/// Download a catalogue model. Returns as soon as the transfer starts; follow
/// `verba:download` for progress.
#[tauri::command]
fn download_model(app: AppHandle, file: String) -> Result<(), String> {
    let entry = config::catalogue()
        .into_iter()
        .find(|m| m.file == file)
        .ok_or_else(|| format!("unknown model: {file}"))?;
    if entry.engine != "whisper.cpp" {
        return Err(format!(
            "{} needs the {} engine, which is not built into this version.",
            entry.name, entry.engine
        ));
    }
    std::thread::Builder::new()
        .name("download".into())
        .spawn(move || download::fetch(&app, &entry.file, &entry.url))
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn reveal_models_dir() -> Result<(), String> {
    let dir = config::models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn hotkey_label(hk: &config::Hotkey) -> String {
    let mut parts = Vec::new();
    if hk.ctrl { parts.push("Ctrl"); }
    if hk.shift { parts.push("Shift"); }
    if hk.alt { parts.push("Alt"); }
    if hk.win { parts.push("Win"); }
    parts.push(&hk.label);
    parts.join("+")
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
    let cfg = config::load();
    let recorder = audio::Recorder::new(cfg.microphone.as_deref())?;
    hotkey::set_binding(cfg.hotkey.vk, cfg.hotkey.mods());
    let events = hotkey::spawn()?;
    let worker = transcribe::spawn()?;

    let overlay = app
        .get_webview_window("overlay")
        .ok_or_else(|| anyhow!("overlay window missing"))?;

    let mut boot = State::new("idle", "");
    boot.visual = Some(cfg.visual.clone());
    boot.model = Some(cfg.model.clone());
    emit(&app, boot);

    if cfg.preload_model {
        // A second of silence: cheap to decode, and it forces the model load
        // now rather than in the middle of the first real dictation.
        let _ = worker.jobs.send(transcribe::Job::Transcribe {
            pcm: vec![0.0; audio::TARGET_RATE as usize],
            utterance: 0,
            final_pass: false,
        });
    }

    log!("ready — hold {} to dictate", hotkey_label(&cfg.hotkey));

    let mut utterance: u64 = 0;
    let mut listening = false;
    let mut mark = 0u64;
    let mut started = Instant::now();
    let mut last_partial = Instant::now();
    let mut last_used = Instant::now();
    let mut hide_at: Option<Instant> = None;
    let mut unloaded = false;

    loop {
        match events.recv_timeout(TICK) {
            Ok(hotkey::Event::Pressed) => {
                utterance += 1;
                mark = recorder.mark();
                started = Instant::now();
                last_partial = Instant::now();
                listening = true;
                hide_at = None;
                unloaded = false;

                // Both fields get logged: writing app-routing rules later means
                // knowing what the exe and title actually look like.
                let info = focus::foreground();
                log!("● listening   [{}] {}", info.exe, info.title);

                let _ = overlay.show();
                let mut s = State::new("listening", "LISTENING");
                s.model = Some(config::load().model);
                emit(&app, s);
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
                let _ = worker.jobs.send(transcribe::Job::Transcribe {
                    pcm,
                    utterance,
                    final_pass: true,
                });
            }

            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Interim passes. Kicked off from here rather than on a timer thread so
        // there is only ever one place deciding what the engine is doing.
        if listening && last_partial.elapsed() >= PARTIAL_EVERY {
            let raw = recorder.take_since(mark);
            let secs = raw.len() as f32 / recorder.sample_rate() as f32;
            if secs >= PARTIAL_MIN_SECS {
                last_partial = Instant::now();
                if let Ok(pcm) = audio::to_16k(&raw, recorder.sample_rate()) {
                    let _ = worker.jobs.send(transcribe::Job::Transcribe {
                        pcm,
                        utterance,
                        final_pass: false,
                    });
                }
            }
        }

        // Results. Drained rather than blocked on, so the visualiser keeps
        // updating while a pass is in flight.
        while let Ok(done) = worker.done.try_recv() {
            match done {
                transcribe::Done::Partial { text, utterance: g } => {
                    // A pass from a previous utterance finishing late must not
                    // overwrite the current one.
                    if g != utterance || !listening || text.is_empty() {
                        continue;
                    }
                    let mut s = State::new("listening", "LISTENING");
                    s.bands = recorder.bands();
                    s.elapsed = started.elapsed().as_secs_f32();
                    s.text = Some(text);
                    s.partial = true;
                    emit(&app, s);
                }

                transcribe::Done::Final { text, utterance: g, took } => {
                    if g != utterance {
                        continue;
                    }
                    last_used = Instant::now();
                    if text.is_empty() {
                        log!("  (silence)");
                        emit(&app, State::new("idle", ""));
                        let _ = overlay.hide();
                        continue;
                    }
                    let held = started.elapsed();
                    log!("  {text}");
                    log!(
                        "  {:.1}s audio · whisper {}ms · {:.1}x realtime",
                        held.as_secs_f32(),
                        took.as_millis(),
                        held.as_secs_f32() / took.as_secs_f32().max(0.001)
                    );

                    let mut s = State::new("transcribing", "INSERTED");
                    s.text = Some(text.clone());
                    emit(&app, s);

                    if let Err(e) = inject::insert(&text) {
                        log!("  insert failed: {e}");
                    }
                    hide_at = Some(Instant::now() + LINGER);
                }

                transcribe::Done::Failed(e) => {
                    log!("  transcription failed: {e}");
                    emit(&app, State::new("idle", ""));
                    let _ = overlay.hide();
                }
            }
        }

        if listening {
            let mut s = State::new("listening", "LISTENING");
            s.bands = recorder.bands();
            s.elapsed = started.elapsed().as_secs_f32();
            emit(&app, s);
        } else if hide_at.is_some_and(|t| Instant::now() >= t) {
            hide_at = None;
            emit(&app, State::new("idle", ""));
            let _ = overlay.hide();
        } else if !unloaded {
            let secs = config::load().model_idle_eject_secs;
            if secs > 0 && last_used.elapsed().as_secs() >= secs {
                unloaded = true;
                let _ = worker.jobs.send(transcribe::Job::Unload);
            }
        }
    }
    Ok(())
}

/// Drive the overlay through its states with no microphone and no model, so the
/// visuals can be checked on their own.
fn overlay_demo(app: AppHandle, visual: &str) {
    let Some(win) = app.get_webview_window("overlay") else { return };
    let _ = win.show();
    log!("overlay demo ({visual}) — listening 10s, then transcript 6s");

    let mut s = State::new("idle", "");
    s.visual = Some(visual.to_string());
    s.model = Some(config::load().model);
    emit(&app, s);
    std::thread::sleep(Duration::from_millis(120));

    const WORDS: &[&str] = &[
        "Thanks", "for", "sending", "the", "deck", "over", "—", "I", "read",
        "the", "pricing", "section", "this", "morning",
    ];

    let start = Instant::now();
    let mut shown = 0usize;
    while start.elapsed() < Duration::from_secs(10) {
        let t = start.elapsed().as_secs_f32();
        let mut s = State::new("listening", "LISTENING");
        // Gated bursts rather than a smooth sweep, so the noise displacement
        // and per-band brightness are actually visible.
        let gate = (t * 0.9).sin().max(0.0).powf(0.6);
        for (i, b) in s.bands.iter_mut().enumerate() {
            let f = 1.1 + i as f32 * 0.83;
            *b = (0.5 + 0.5 * (t * f + i as f32 * 2.3).sin()).powf(1.5) * gate;
        }
        s.elapsed = t;
        // Words accumulate the way interim passes deliver them.
        let want = ((t / 10.0) * WORDS.len() as f32) as usize;
        if want > shown {
            shown = want.min(WORDS.len());
            s.text = Some(WORDS[..shown].join(" "));
            s.partial = true;
        }
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

    // `verba --download <name-fragment>` — headless model fetch, and the way
    // the download path gets exercised without a window.
    if arg1 == Some("--download") {
        let want = args.get(2).map(String::as_str).unwrap_or("");
        let Some(m) = config::catalogue()
            .into_iter()
            .find(|m| m.engine == "whisper.cpp" && m.file.contains(want))
        else {
            log!("no whisper.cpp model matching '{want}'. Options:");
            for m in config::catalogue().iter().filter(|m| m.engine == "whisper.cpp") {
                log!("  {} ({} MB)", m.file, m.size_mb);
            }
            return Ok(());
        };
        log!("fetching {} ({} MB)", m.file, m.size_mb);
        let mut last_pct = u64::MAX;
        return match download::fetch_with(&m.file, &m.url, |got, total| {
            let pct = if total > 0 { got * 100 / total } else { 0 };
            if pct != last_pct {
                last_pct = pct;
                log!("  {pct}%  {} / {} MB", got / 1048576, total / 1048576);
            }
        }) {
            Ok(p) => {
                log!("saved to {}", p.display());
                Ok(())
            }
            Err(e) => Err(anyhow!("download failed: {e}")),
        };
    }

    let demo = (arg1 == Some("--overlay-test"))
        .then(|| args.get(2).cloned().unwrap_or_else(|| config::load().visual));
    let open_settings = arg1 == Some("--settings");

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_config,
            download_model,
            reveal_models_dir
        ])
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
                .tooltip("Verba — hold your hotkey to dictate")
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
            // The audio stream is !Send on Windows, so the engine owns the
            // recorder and hotkey receiver together on one thread.
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
