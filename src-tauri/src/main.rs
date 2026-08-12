//! Verba — local-first speech to text.
//!
//! The engine runs on its own thread and emits state; the overlay is a pure
//! consumer of that state and holds no logic of its own.

// GUI subsystem: no console window when launched from Explorer. log::init()
// attaches to the parent terminal when there is one, so CLI use still works.
#![windows_subsystem = "windows"]

#[macro_use]
mod log;

mod accent;
mod audio;
mod capture;
mod childguard;
mod config;
mod download;
mod fasterwhisper;
mod focus;
mod hardware;
mod hotkey;
mod inject;
mod llm;
mod ollama;
mod overlay;
mod pipeline;
mod parakeet;
mod startup;
mod stt;
mod learn;
mod packs;
mod transcribe;
mod update;

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use std::os::windows::process::CommandExt;
use windows::Win32::Foundation::HWND;

const EVENT: &str = "verba:state";
/// Below this, it's a stray keypress rather than speech.
const MIN_SPEECH_MS: u128 = 300;
/// How long the transcript stays up after insertion, to read back what landed.
const LINGER: Duration = Duration::from_millis(2600);
/// Level updates while listening. ~30fps is plenty for a wave this soft.
const TICK: Duration = Duration::from_millis(33);
/// Interim-pass cadence, adapted to how long a pass actually takes. A fixed
/// interval has to be set for the slowest engine, which wastes Parakeet — it
/// decodes in about 90ms where whisper small takes 300. Held at roughly three
/// times the measured cost so the worker stays mostly idle and the final pass
/// never queues behind a backlog.
const PARTIAL_MIN: Duration = Duration::from_millis(280);
const PARTIAL_MAX: Duration = Duration::from_millis(1200);
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
    /// Formatting mode that produced the inserted text.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    gpu: bool,
    /// Downscaled screenshot of what sits behind the overlay, base64 RGBA.
    /// Sent once when listening starts — it is tens of kilobytes, not
    /// something to put on the 30fps tick.
    #[serde(skip_serializing_if = "Option::is_none")]
    backdrop: Option<Backdrop>,
    /// Windows accent, sent once at boot so both treatments can tint to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    accent: Option<accent::Accent>,
}

#[derive(Clone, serde::Serialize)]
struct Backdrop {
    width: u32,
    height: u32,
    rgba: String,
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
            mode: None,
            gpu: cfg!(feature = "gpu-vulkan"),
            backdrop: None,
            accent: None,
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
    let p = config::models_dir().join(config::safe_model_name(file)?);
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
    accent: accent::Accent,
    llm_models: Vec<ollama::LlmModel>,
    ollama_status: ollama::Status,
    llm_recommended: &'static str,
    /// The running build, so the settings window can say what it is and the
    /// update card has something to compare against.
    version: &'static str,
    /// True when a verified newer binary is already downloaded and will be
    /// swapped in at the next idle moment.
    update_staged: bool,
}

#[tauri::command]
fn get_state() -> SettingsState {
    let hw = hardware::detect();
    let hw2 = hw.clone();
    let cfg_now = config::load();
    SettingsState {
        config: config::load(),
        recommendation: hardware::recommend(&hw),
        // Rated for this machine: the same model is quick on a GPU and slow
        // without one, and the speed bar has to say which of those the user is
        // actually looking at.
        models: config::catalogue_for(&hw),
        hardware: hw,
        microphones: audio::input_devices(),
        // Every known engine is listed with an availability flag. Unbuilt ones
        // are shown greyed rather than hidden, so the roadmap is visible
        // without offering a choice that would silently do nothing.
        engines: config::engines(),
        log_path: log::path().display().to_string(),
        config_path: config::path().display().to_string(),
        models_dir: config::models_dir().display().to_string(),
        accent: accent::detect(),
        llm_models: ollama::catalogue(&cfg_now, &hw2),
        ollama_status: ollama::status(&cfg_now),
        llm_recommended: ollama::recommended_for(&hw2),
        version: env!("CARGO_PKG_VERSION"),
        update_staged: update::staged(),
    }
}

/// The text of the most recent dictation, as inserted. In memory only: this is
/// the user's words, and keeping a copy on disk is exactly what the correction
/// history opt-in exists to ask permission for.
static LAST_DICTATION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[tauri::command]
fn last_dictation() -> Option<String> {
    LAST_DICTATION.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Save a corrected version of the last dictation and learn from the diff.
#[tauri::command]
fn record_correction(fixed: String) -> Result<Vec<learn::Learned>, String> {
    if !config::load().learn_from_corrections {
        return Err("learning from corrections is switched off".into());
    }
    let raw = last_dictation().ok_or("nothing has been dictated yet")?;
    let pairs = learn::record(&raw, &fixed)?;
    log!("correction recorded: {} substitution(s)", pairs.len());
    // The corrected text becomes the new baseline, so a second pass over the
    // same dictation does not re-learn the edits already saved.
    *LAST_DICTATION.lock().unwrap_or_else(|e| e.into_inner()) = Some(fixed);
    Ok(learn::learned())
}

#[tauri::command]
fn learned_corrections() -> Vec<learn::Learned> {
    learn::learned()
}

#[tauri::command]
fn clear_corrections() -> Result<(), String> {
    learn::clear()
}

/// Every pack, built-in and user-authored, for the packs panel.
#[tauri::command]
fn list_packs() -> Vec<packs::Pack> {
    packs::all()
}

/// Ask GitHub whether there is a newer release. Returns null when current.
#[tauri::command]
fn check_update() -> Result<Option<update::Available>, String> {
    update::check()
}

/// Download and verify a release, leaving it staged for the next idle moment.
/// The transfer runs off the UI thread and reports through `verba:update`.
#[tauri::command]
fn download_update(app: AppHandle, avail: update::Available) -> Result<(), String> {
    std::thread::Builder::new()
        .name("update-dl".into())
        .spawn(move || {
            let version = avail.version.clone();
            let emit_progress = |received: u64, total: u64| {
                let _ = app.emit(
                    update::EVENT,
                    serde_json::json!({
                        "version": version, "received": received,
                        "total": total, "done": false
                    }),
                );
            };
            let result = update::stage(&avail, emit_progress);
            let _ = app.emit(
                update::EVENT,
                match &result {
                    Ok(_) => serde_json::json!({
                        "version": avail.version, "done": true, "staged": true
                    }),
                    Err(e) => serde_json::json!({
                        "version": avail.version, "done": true, "error": e
                    }),
                },
            );
            match result {
                Ok(_) => log!("update {} staged", avail.version),
                Err(e) => log!("update {} failed: {e}", avail.version),
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Swap the staged binary in and restart into it, now, at the user's request.
#[tauri::command]
fn apply_update(app: AppHandle) -> Result<(), String> {
    update::apply()?;
    log!("restarting into the new version");
    app.exit(0);
    Ok(())
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
    if entry.url.is_empty() {
        return Err(format!("{} downloads itself on first use", entry.name));
    }
    if entry.engine == "parakeet" && !parakeet::is_installed() {
        return Err("Install the Parakeet engine first".into());
    }

    std::thread::Builder::new()
        .name("download".into())
        .spawn(move || {
            // sherpa-onnx models are archives of ONNX files, so the download
            // lands as .tar.bz2 and is unpacked into a directory named after
            // the archive.
            let archive = if entry.engine == "parakeet" {
                format!("{}.tar.bz2", entry.file)
            } else {
                entry.file.clone()
            };
            download::fetch_named(&app, &entry.file, &archive, &entry.url, |path| {
                if entry.engine == "parakeet" {
                    parakeet::extract(path)
                } else {
                    Ok(())
                }
            });
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Set up an engine that Verba can install itself. Returns immediately; follow
/// `verba:engine` for progress.
#[tauri::command]
fn install_engine(app: AppHandle, id: String) -> Result<(), String> {
    if !config::installable_engines().contains(&id.as_str()) {
        return Err(format!("{id} cannot be installed from here"));
    }
    std::thread::Builder::new()
        .name("engine-install".into())
        .spawn(move || {
            let name = id.clone();
            let say = |msg: &str, done: bool, error: Option<String>| {
                let _ = app.emit(
                    "verba:engine",
                    serde_json::json!({ "id": name, "message": msg,
                                        "done": done, "error": error }),
                );
            };
            let report = |m: &str| {
                log!("{id}: {m}");
                say(m, false, None);
            };
            let result = match id.as_str() {
                "parakeet" => parakeet::install(report),
                _ => fasterwhisper::install(report),
            };
            match result {
                Ok(()) => say("Installed", true, None),
                Err(e) => {
                    log!("{id} install failed: {e}");
                    say("Failed", true, Some(e.to_string()));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Start Ollama on demand from the settings window, so the status line can be
/// acted on rather than just read.
#[tauri::command]
fn start_ollama() -> Result<String, String> {
    let cfg = config::load();
    ollama::ensure_running(&cfg).map_err(|e| e.to_string())?;
    Ok("running".into())
}

/// Pull an Ollama model. Returns once the transfer starts; follow
/// `verba:llm-pull` for progress.
#[tauri::command]
fn pull_llm_model(app: AppHandle, name: String) -> Result<(), String> {
    std::thread::Builder::new()
        .name("llm-pull".into())
        .spawn(move || {
            let cfg = config::load();
            let say = |status: &str, done: u64, total: u64, finished: bool, err: Option<String>| {
                let _ = app.emit(
                    "verba:llm-pull",
                    serde_json::json!({
                        "name": name, "status": status, "completed": done,
                        "total": total, "done": finished, "error": err,
                    }),
                );
            };
            match ollama::pull(&cfg, &name, |status, done, total| say(status, done, total, false, None)) {
                Ok(()) => {
                    log!("pulled {name}");
                    say("ready", 0, 0, true, None);
                }
                Err(e) => {
                    log!("pull failed for {name}: {e}");
                    say("failed", 0, 0, true, Some(e.to_string()));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Open a link in the user's browser. Restricted to an allow-list: this takes
/// a string from the frontend and hands it to the shell, so anything less
/// would be a way to launch arbitrary targets.
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    const ALLOWED: &[&str] = &["https://ollama.com/download", "https://ollama.com"];
    if !ALLOWED.contains(&url.as_str()) {
        return Err(format!("refusing to open {url}"));
    }
    std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .creation_flags(0x0800_0000)
        .spawn()
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

/// Minimal 16-bit PCM WAV reader, for the --transcribe diagnostic. Walks the
/// chunk list rather than assuming a 44-byte header, since plenty of files
/// carry LIST or fact chunks before the data.
fn read_wav(path: &std::path::Path) -> Result<(Vec<f32>, u32)> {
    let b = std::fs::read(path)?;
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(anyhow!("not a RIFF/WAVE file"));
    }
    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32::from_le_bytes(b[pos + 4..pos + 8].try_into()?) as usize;
        let body = pos + 8;
        match id {
            b"fmt " if body + 16 <= b.len() => {
                channels = u16::from_le_bytes(b[body + 2..body + 4].try_into()?);
                rate = u32::from_le_bytes(b[body + 4..body + 8].try_into()?);
                bits = u16::from_le_bytes(b[body + 14..body + 16].try_into()?);
            }
            b"data" => {
                if bits != 16 {
                    return Err(anyhow!("only 16-bit PCM is supported, got {bits}-bit"));
                }
                let end = (body + size).min(b.len());
                let ch = channels.max(1) as usize;
                let samples: Vec<f32> = b[body..end]
                    .chunks_exact(2)
                    .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0)
                    .collect();
                // Downmix, matching what the recorder does live.
                let mono = samples
                    .chunks(ch)
                    .map(|f| f.iter().sum::<f32>() / ch as f32)
                    .collect();
                return Ok((mono, rate));
            }
            _ => {}
        }
        pos = body + size + (size & 1); // chunks are word-aligned
    }
    Err(anyhow!("no data chunk in {}", path.display()))
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

/// Set once a verified newer binary is waiting. The engine loop reads it and
/// performs the swap the next time it is idle, so an update can never land in
/// the middle of a dictation.
static UPDATE_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Poll GitHub for a newer release, download it, and arm the swap.
///
/// Deliberately unhurried: the first check waits, because startup already has a
/// model load and a possible LLM warm-up competing for the disk, and nobody
/// needs an update in the first thirty seconds of a session.
fn spawn_update_watch(app: AppHandle) {
    /// Long enough to stay out of the way of everything else starting.
    const FIRST_CHECK: Duration = Duration::from_secs(45);
    /// GitHub allows 60 unauthenticated calls an hour; this is four a day.
    const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

    std::thread::Builder::new()
        .name("update-watch".into())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK);
            loop {
                // Re-read rather than capturing: turning auto-update off in
                // settings has to take effect without a restart.
                if !config::load().auto_update {
                    std::thread::sleep(EVERY);
                    continue;
                }

                if update::staged() {
                    // Already downloaded and waiting for an idle moment.
                    UPDATE_READY.store(true, std::sync::atomic::Ordering::Relaxed);
                } else {
                    match update::check() {
                        Ok(Some(avail)) => {
                            log!("update available: {} (running {})",
                                 avail.version, env!("CARGO_PKG_VERSION"));
                            match update::stage(&avail, |_, _| {}) {
                                Ok(_) => {
                                    log!("update {} staged; will apply when idle", avail.version);
                                    let _ = app.emit(update::EVENT, serde_json::json!({
                                        "version": avail.version, "done": true, "staged": true
                                    }));
                                    UPDATE_READY.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                // Not worth surfacing: the app keeps working on
                                // the version it has, and the next pass retries.
                                Err(e) => log!("update {} not staged: {e}", avail.version),
                            }
                        }
                        Ok(None) => {}
                        Err(e) => log!("update check failed: {e}"),
                    }
                }
                std::thread::sleep(EVERY);
            }
        })
        .ok();
}

/// True when the window that had focus belongs to this process's own exe.
/// `focus::foreground()` reports the bare file name, so this compares like
/// with like.
fn is_own_window(exe: &str) -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
        .is_some_and(|own| own.eq_ignore_ascii_case(exe))
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
    // Refreshed once per dictation rather than per tick. Re-reading it in the
    // idle branch meant a file open, read and full JSON parse thirty times a
    // second for as long as the app was running.
    let mut cfg = config::load();
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
    boot.accent = Some(accent::detect());
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

    // Warm the rewrite model off the critical path. Ollama loads weights on
    // first request, not at startup, so without this the first dictation after
    // a cold start waits through the load and times out into the fallback.
    if !cfg.llm_model.trim().is_empty() && cfg.modes.iter().any(|m| m.llm) {
        let warm = cfg.clone();
        std::thread::Builder::new()
            .name("llm-warm".into())
            .spawn(move || match ollama::preload(&warm) {
                Ok(()) => log!("rewrite model {} warm", warm.llm_model),
                // Not an error worth surfacing: post-processing degrades to the
                // cleaned transcript, which is a usable outcome on its own.
                Err(e) => log!("rewrite model not warmed: {e}"),
            })
            .ok();
    }

    log!("ready — hold {} to dictate", hotkey_label(&cfg.hotkey));

    let mut utterance: u64 = 0;
    let mut listening = false;
    let mut mark = 0u64;
    let mut started = Instant::now();
    let mut last_partial = Instant::now();
    let mut last_used = Instant::now();
    // Seeded pessimistically; the first completed pass replaces it.
    let mut partial_cost = Duration::from_millis(300);
    let mut partial_sent: Option<Instant> = None;
    let mut hide_at: Option<Instant> = None;
    let mut unloaded = false;
    let mut target = focus::App::default();

    loop {
        match events.recv_timeout(TICK) {
            Ok(hotkey::Event::Pressed) => {
                utterance += 1;
                mark = recorder.mark();
                started = Instant::now();
                last_partial = Instant::now();
                // The model is about to be needed, so the idle clock restarts
                // here. Updating it only on a completed pass let the eject fire
                // in the gap between release and result.
                last_used = Instant::now();
                listening = true;
                hide_at = None;
                unloaded = false;
                cfg = config::load();

                // Captured at press, not at insert: this is where the user was
                // looking when they started speaking, and it decides which
                // formatting mode applies.
                target = focus::foreground();
                let mode = cfg.mode_for(&target.exe, &target.title);
                log!("● listening   [{}] {} → {}", target.exe, target.title, mode.name);

                let mut s = State::new("listening", "LISTENING");
                s.model = Some(cfg.model.clone());
                // Only the ribbons panel paints a backdrop; for the other
                // treatments the capture, the base64 and the 26KB of IPC would
                // all be discarded.
                if cfg.visual == "ribbons" {
                    // Capture before showing, so the shot does not contain the
                    // overlay itself — otherwise the panel blurs a picture of
                    // its own previous frame.
                    if let Ok(h) = overlay.hwnd() {
                        if let Some(shot) = capture::behind(HWND(h.0 as _)) {
                            s.backdrop = Some(Backdrop {
                                width: shot.width,
                                height: shot.height,
                                rgba: capture::base64(&shot.rgba),
                            });
                        }
                    }
                }
                let _ = overlay.show();
                emit(&app, s);
            }

            Ok(hotkey::Event::Released) => {
                listening = false;
                // Any interim pass still in flight belongs to this utterance
                // and is about to be superseded.
                partial_sent = None;
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
                last_used = Instant::now();
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
        let interval = (partial_cost * 3).clamp(PARTIAL_MIN, PARTIAL_MAX);
        if listening && partial_sent.is_none() && last_partial.elapsed() >= interval {
            let raw = recorder.take_since(mark);
            let secs = raw.len() as f32 / recorder.sample_rate() as f32;
            if secs >= PARTIAL_MIN_SECS {
                last_partial = Instant::now();
                if let Ok(pcm) = audio::to_16k(&raw, recorder.sample_rate()) {
                    partial_sent = Some(Instant::now());
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
        let mut interim: Option<String> = None;
        while let Ok(done) = worker.done.try_recv() {
            match done {
                transcribe::Done::Partial { text, utterance: g } => {
                    if let Some(sent) = partial_sent.take() {
                        partial_cost = sent.elapsed();
                    }
                    // A pass from a previous utterance finishing late must not
                    // overwrite the current one.
                    if g != utterance || !listening || text.is_empty() {
                        continue;
                    }
                    // Handed to the state the listening block below already
                    // builds. Emitting here instead meant a second FFT and a
                    // second event that the next one immediately overwrote.
                    interim = Some(text);
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
                        "  {:.1}s audio · decode {}ms · {:.1}x realtime",
                        held.as_secs_f32(),
                        took.as_millis(),
                        held.as_secs_f32() / took.as_secs_f32().max(0.001)
                    );

                    // Formatting runs inline. Raw mode is deterministic and
                    // effectively free; only a mode with the model pass on can
                    // block here, and by then the user has stopped speaking so
                    // there is nothing to keep responsive.
                    let uses_model = cfg.mode_for(&target.exe, &target.title).llm;
                    if uses_model {
                        emit(&app, State::new("transcribing", "FORMATTING"));
                    }
                    let t1 = Instant::now();
                    let (formatted, mode_name) =
                        pipeline::run(&text, &cfg, &target.exe, &target.title);
                    if uses_model {
                        log!("  {mode_name} · {}ms", t1.elapsed().as_millis());
                        log!("  {formatted}");
                    }

                    // Kept so "fix last transcription" has something to diff
                    // against. Held in memory only — nothing is written to disk
                    // unless the user actually corrects it and has opted in.
                    *LAST_DICTATION.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(formatted.clone());

                    let mut s = State::new("transcribing", "INSERTED");
                    s.text = Some(formatted.clone());
                    s.mode = Some(mode_name);
                    emit(&app, s);

                    // Verba's own windows are never an insertion target. The
                    // onboarding try-out has the user dictate with this app
                    // focused on purpose, and synthesising unicode into our own
                    // WebView would at best go nowhere and at worst trip its
                    // key handling. The overlay still shows the text, which is
                    // the whole point of that step.
                    if is_own_window(&target.exe) {
                        log!("  not inserted: Verba had focus");
                    } else if let Err(e) = inject::insert(&formatted) {
                        log!("  insert failed: {e}");
                    }
                    hide_at = Some(Instant::now() + LINGER);
                }

                transcribe::Done::Failed { error, utterance: g } => {
                    log!("  transcription failed: {error}");
                    // Clear the in-flight marker, or the `partial_sent.is_none()`
                    // guard blocks every further interim pass this dictation.
                    partial_sent = None;
                    // A failure from a finished utterance, or one from an
                    // interim pass while the user is still speaking, must not
                    // tear the overlay down mid-sentence.
                    if g == utterance && !listening {
                        emit(&app, State::new("idle", ""));
                        let _ = overlay.hide();
                    }
                }
            }
        }

        if listening {
            let mut s = State::new("listening", "LISTENING");
            s.bands = recorder.bands();
            s.elapsed = started.elapsed().as_secs_f32();
            if let Some(text) = interim {
                s.text = Some(text);
                s.partial = true;
            }
            emit(&app, s);
        } else if hide_at.is_some_and(|t| Instant::now() >= t) {
            hide_at = None;
            emit(&app, State::new("idle", ""));
            let _ = overlay.hide();
        } else if !unloaded && cfg.model_idle_eject_secs > 0 {
            if last_used.elapsed().as_secs() >= cfg.model_idle_eject_secs {
                unloaded = true;
                let _ = worker.jobs.send(transcribe::Job::Unload);
            }
        }

        // Apply a staged update only from here: this branch is reached with no
        // utterance in flight and the overlay already hidden. Restarting a
        // tray app in that state costs the user nothing, whereas doing it a
        // moment earlier would drop whatever they had just said.
        //
        // The grace period is against the case where someone dictates in
        // bursts — releasing the hotkey briefly between sentences should not
        // be read as "done for the day".
        if UPDATE_READY.load(std::sync::atomic::Ordering::Relaxed)
            && !listening
            && hide_at.is_none()
            && last_used.elapsed() >= UPDATE_IDLE_GRACE
        {
            match update::apply() {
                Ok(()) => {
                    log!("applied staged update, restarting");
                    app.exit(0);
                    return Ok(());
                }
                Err(e) => {
                    // Clear the flag: a failure here is not transient — the
                    // directory is read-only, or the staged file went missing —
                    // and retrying every tick would fill the log.
                    UPDATE_READY.store(false, std::sync::atomic::Ordering::Relaxed);
                    log!("could not apply update: {e}");
                }
            }
        }
    }
    Ok(())
}

/// How long the machine must be free of dictation before an update restarts
/// the app underneath the user.
const UPDATE_IDLE_GRACE: Duration = Duration::from_secs(120);

/// Drive the overlay through its states with no microphone and no model, so the
/// visuals can be checked on their own.
fn overlay_demo(app: AppHandle, visual: &str) {
    let Some(win) = app.get_webview_window("overlay") else { return };
    let _ = win.show();
    log!("overlay demo ({visual}) — listening 10s, then transcript 6s");

    let mut s = State::new("idle", "");
    s.visual = Some(visual.to_string());
    s.model = Some(config::load().model);
    s.accent = Some(accent::detect());
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

    // `verba --format "<text>" [exe] [title]` — run the formatting pipeline on
    // text, as if it had been dictated into that app. Exercises mode routing,
    // the deterministic rules and the model pass without a microphone.
    if arg1 == Some("--format") {
        let Some(text) = args.get(2) else {
            return Err(anyhow!("usage: verba --format \"<text>\" [exe] [window title]"));
        };
        let cfg = config::load();
        let exe = args.get(3).cloned().unwrap_or_default();
        let title = args.get(4).cloned().unwrap_or_default();
        let mode = cfg.mode_for(&exe, &title);
        log!(
            "app   [{}] {}\nmode  {} (model pass: {})",
            if exe.is_empty() { "none" } else { &exe },
            title,
            mode.name,
            if mode.llm { cfg.llm_model.as_str() } else { "off" }
        );
        log!("\nraw       {text}");
        log!("cleaned   {}", pipeline::clean(text, &cfg, mode));
        let t0 = Instant::now();
        let (out, label) = pipeline::run(text, &cfg, &exe, &title);
        log!("inserted  {out}");
        log!("\n{label} in {}ms", t0.elapsed().as_millis());
        return Ok(());
    }

    // `verba --meters [secs]` — live spectrum from the real microphone, drawn
    // in the console. The only way to tell whether the visualiser looks dead
    // because of the rendering or because the numbers reaching it are dead.
    if arg1 == Some("--meters") {
        let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(12);
        let rec = audio::Recorder::new(config::load().microphone.as_deref())?;
        log!("speak — {secs}s of live band energy\n");
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(secs) {
            let bands = rec.bands();
            let bar: String = bands
                .iter()
                .map(|&v| {
                    // Eight levels of block, so a glance shows the shape.
                    const R: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
                    R[((v * 8.0).round() as usize).min(8)]
                })
                .collect();
            let peak = bands.iter().cloned().fold(0.0f32, f32::max);
            let mean = bands.iter().sum::<f32>() / bands.len() as f32;
            log!("[{bar}] peak {peak:.2} mean {mean:.2}");
            std::thread::sleep(Duration::from_millis(120));
        }
        return Ok(());
    }

    // `verba --install-engine <id>` — same bootstrap the settings window runs,
    // without a window.
    if arg1 == Some("--install-engine") {
        let id = args.get(2).map(String::as_str).unwrap_or("");
        let report = |m: &str| log!("  {m}");
        return match id {
            "parakeet" => parakeet::install(report),
            "faster-whisper" => fasterwhisper::install(report),
            _ => Err(anyhow!("unknown engine '{id}' (parakeet | faster-whisper)")),
        };
    }

    // `verba --transcribe <file.wav>` — run a real clip through whichever
    // engine and model are configured. Exercises the whole Rust-to-backend
    // path, which unit tests cannot reach.
    if arg1 == Some("--transcribe") {
        let Some(path) = args.get(2) else {
            return Err(anyhow!("usage: verba --transcribe <file.wav>"));
        };
        let cfg = config::load();
        let (pcm, rate) = read_wav(std::path::Path::new(path))?;
        let pcm = audio::to_16k(&pcm, rate)?;
        log!(
            "{:.1}s of audio · engine {} · model {}",
            pcm.len() as f32 / audio::TARGET_RATE as f32,
            cfg.engine,
            cfg.model
        );

        let worker = transcribe::spawn()?;
        let t0 = Instant::now();
        worker.jobs.send(transcribe::Job::Transcribe {
            pcm,
            utterance: 1,
            final_pass: true,
        })?;
        return match worker.done.recv()? {
            transcribe::Done::Final { text, took, .. } => {
                log!("\n  {text}\n");
                log!("decoded in {}ms (total {}ms incl. load)", took.as_millis(), t0.elapsed().as_millis());
                Ok(())
            }
            transcribe::Done::Failed { error, .. } => Err(anyhow!("{error}")),
            _ => Err(anyhow!("unexpected reply")),
        };
    }

    // `verba --accent` — what the settings window will paint with. UISettings
    // is a WinRT call and can fail without an apartment, so this shows whether
    // the real source answered or the registry fallback did.
    if arg1 == Some("--accent") {
        let a = accent::detect();
        log!("base   {}   rgb({})", a.base, a.rgb);
        log!("light1 {}", a.light1);
        log!("light2 {}  <- accent text on dark", a.light2);
        log!("light3 {}", a.light3);
        log!("dark1  {}", a.dark1);
        return Ok(());
    }

    // `verba --fix "<what it heard>" "<what you meant>"` — record a correction
    // without the UI, and show what it changed. The only way to exercise the
    // learning loop end to end without dictating the same sentence three times.
    if arg1 == Some("--fix") {
        let (Some(raw), Some(fixed)) = (args.get(2), args.get(3)) else {
            log!("usage: verba --fix \"<heard>\" \"<meant>\"");
            return Ok(());
        };
        // The same gate the UI honours. A diagnostic that quietly starts a
        // history file the user has not asked for would defeat the opt-in.
        if !config::load().learn_from_corrections {
            log!("learning from corrections is off; enable it in Settings first");
            return Ok(());
        }
        let pairs = learn::record(raw, fixed).map_err(|e| anyhow!(e))?;
        log!("learned {} substitution(s):", pairs.len());
        for (w, r) in &pairs {
            log!("  {w:?} -> {r:?}");
        }
        return Ok(());
    }

    // `verba --learned` — the aggregate, and exactly what each engine will do
    // with it. Answers "why has this not started working yet".
    if arg1 == Some("--learned") {
        let cfg = config::load();
        log!("learning: {}", if cfg.learn_from_corrections { "on" } else { "OFF" });
        log!("history:  {}", learn::path().display());
        let all = learn::learned();
        if all.is_empty() {
            log!("nothing learned yet");
        }
        for l in &all {
            let how = match (l.promoted, l.rewrite) {
                (_, true) => "rewrite + bias",
                (true, false) => "bias only",
                _ => "not yet",
            };
            log!("  {:>2}x  {:?} -> {:?}   [{how}]", l.count, l.wrong, l.right);
        }
        log!("");
        log!("packs enabled: {:?}", cfg.enabled_packs);
        let bias = cfg.bias_terms();
        log!("bias terms ({}): {}", bias.len(), bias.join(", "));
        log!("");
        log!(
            "engine {} {} use bias",
            cfg.engine,
            if cfg.engine == "parakeet" { "cannot" } else { "can" }
        );
        return Ok(());
    }

    // `verba --check-update` — what the background watcher sees, without
    // waiting 45 seconds for it or restarting anything.
    if arg1 == Some("--check-update") {
        log!("running {}", env!("CARGO_PKG_VERSION"));
        match update::check().map_err(|e| anyhow!(e))? {
            None => log!("up to date"),
            Some(a) => {
                log!("available  {}", a.version);
                log!("  url      {}", a.url);
                log!("  size     {:.1} MB", a.size as f64 / 1_048_576.0);
                log!("  sha256   {}", a.sha256);
            }
        }
        log!("staged: {}", update::staged());
        return Ok(());
    }

    // `verba --self-update` — the whole cycle on demand: check, download,
    // verify, swap, relaunch. Also the only way to exercise the swap without
    // waiting for a real release to appear.
    if arg1 == Some("--self-update") {
        let Some(avail) = update::check().map_err(|e| anyhow!(e))? else {
            log!("already on {}, nothing to do", env!("CARGO_PKG_VERSION"));
            return Ok(());
        };
        log!("downloading {} ({:.1} MB)", avail.version, avail.size as f64 / 1_048_576.0);

        let mut last = 0u64;
        update::stage(&avail, |received, total| {
            // One line per 10%, so a slow link still shows life without
            // scrolling the log off the screen.
            let step = (total / 10).max(1);
            if received - last >= step {
                last = received;
                log!("  {:>3}%", received * 100 / total.max(1));
            }
        })
        .map_err(|e| anyhow!(e))?;

        log!("verified, swapping in");
        update::apply().map_err(|e| anyhow!(e))?;
        log!("now running {}", avail.version);
        return Ok(());
    }

    // `verba --state` — the exact payload the settings window renders from.
    // The window is a separate WebView, so when a panel comes up empty this is
    // the only way to tell a bad payload from a bad render.
    if arg1 == Some("--state") {
        // log!, not println!: this is a windows-subsystem binary, so stdout
        // goes nowhere unless a console is attached.
        log!("{}", serde_json::to_string_pretty(&get_state())?);
        return Ok(());
    }

    // `verba --capture-test` — grab the region the overlay covers and report
    // what came back. A capture that silently returns black looks exactly like
    // a blur that isn't working.
    if arg1 == Some("--capture-test") {
        match capture::screen_region(0, 0, 760, 420) {
            Some(shot) => {
                let px = shot.rgba.len() / 4;
                let mut min = 255u8;
                let mut max = 0u8;
                let mut sum = 0u64;
                for c in shot.rgba.chunks_exact(4) {
                    let lum = ((c[0] as u32 * 3 + c[1] as u32 * 6 + c[2] as u32) / 10) as u8;
                    min = min.min(lum);
                    max = max.max(lum);
                    sum += lum as u64;
                }
                log!("captured {}x{} ({px} px)", shot.width, shot.height);
                log!("luminance min {min} max {max} mean {}", sum / px.max(1) as u64);
                log!(
                    "base64 {} chars — {}",
                    capture::base64(&shot.rgba).len(),
                    if max > min { "real image" } else { "FLAT: capture returned nothing" }
                );
            }
            None => log!("capture failed"),
        }
        return Ok(());
    }

    // `verba --download <name-fragment>` — headless model fetch, and the way
    // the download path gets exercised without a window.
    if arg1 == Some("--download") {
        let want = args.get(2).map(String::as_str).unwrap_or("");
        let Some(m) = config::catalogue()
            .into_iter()
            .find(|m| !m.url.is_empty() && m.file.contains(want))
        else {
            log!("no downloadable model matching '{want}'. Options:");
            for m in config::catalogue().iter().filter(|m| !m.url.is_empty()) {
                log!("  [{}] {} ({} MB)", m.engine, m.file, m.size_mb);
            }
            return Ok(());
        };

        // sherpa-onnx models arrive as archives and are unpacked into a
        // directory; GGML models are the file itself.
        let archive = if m.engine == "parakeet" {
            format!("{}.tar.bz2", m.file)
        } else {
            m.file.clone()
        };
        log!("fetching {} ({} MB) for {}", m.file, m.size_mb, m.engine);

        let mut last_pct = u64::MAX;
        let got = download::fetch_with(&archive, &m.url, |got, total| {
            let pct = if total > 0 { got * 100 / total } else { 0 };
            if pct != last_pct {
                last_pct = pct;
                log!("  {pct}%  {} / {} MB", got / 1048576, total / 1048576);
            }
        });
        return match got {
            Ok(p) => {
                if m.engine == "parakeet" {
                    log!("extracting…");
                    parakeet::extract(&p)?;
                    log!("installed to {}", config::models_dir().join(&m.file).display());
                } else {
                    log!("saved to {}", p.display());
                }
                Ok(())
            }
            Err(e) => Err(anyhow!("download failed: {e}")),
        };
    }

    let demo = (arg1 == Some("--overlay-test"))
        .then(|| args.get(2).cloned().unwrap_or_else(|| config::load().visual));
    let open_settings = arg1 == Some("--settings");
    // `--onboard` replays the first-run flow without editing the config by
    // hand, which is the only practical way to test changes to it.
    let show_onboarding = arg1 == Some("--onboard") || !config::load().onboarded;

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_config,
            download_model,
            install_engine,
            start_ollama,
            pull_llm_model,
            open_url,
            reveal_models_dir,
            check_update,
            download_update,
            apply_update,
            last_dictation,
            record_correction,
            learned_corrections,
            clear_corrections,
            list_packs
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

            // First run, or an upgrade from before the flag existed. Shown
            // here rather than from the engine thread so it appears while the
            // model is still loading instead of after it.
            if show_onboarding {
                if let Some(w) = app.get_webview_window("onboard") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }

            // The binary we replaced last time, no longer running and so
            // finally deletable.
            update::sweep();

            if config::load().auto_update {
                spawn_update_watch(app.handle().clone());
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

#[cfg(test)]
mod tests {
    use super::is_own_window;

    /// The guard that stops a dictation typing into Verba's own WebView. It
    /// compares bare file names because that is what focus::foreground()
    /// reports, and Windows paths are case-insensitive.
    #[test]
    fn own_window_is_recognised_by_name() {
        let own = std::env::current_exe().unwrap();
        let name = own.file_name().unwrap().to_string_lossy().into_owned();

        assert!(is_own_window(&name), "the running exe must match itself");
        assert!(is_own_window(&name.to_uppercase()), "matching must ignore case");

        assert!(!is_own_window("notepad.exe"));
        assert!(!is_own_window(""), "an unreadable foreground exe must not count as ours");
        // A full path is not what foreground() returns; if that ever changes,
        // this guard would silently stop firing and dictation would type into
        // our own window again.
        assert!(!is_own_window(&own.to_string_lossy()));
    }
}
