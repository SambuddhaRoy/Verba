# Verba — plan

A free, local-first SuperWhisper alternative for Windows 11. Hold `Ctrl+Shift+Space`,
speak, release, and text lands at your caret — formatted differently depending on
which app you're in.

## Context

SuperWhisper is macOS-only and paid. The two things worth copying are its *feel*
(sub-second, no window management, text just appears) and its *modes* (the same
dictation becomes an email, a code comment, or bullet notes depending on context).
Everything else is incidental.

Two constraints shape this build:

- **Local models.** Transcription and reformatting run on-device. Cloud is a
  pluggable fallback, never the default.
- **Output formatting is the product.** Not transcription accuracy — whisper already
  solved that. The differentiator is that dictating into VS Code produces different
  text than dictating into Outlook, without the user picking a mode.

The visual language comes from `Halo Mockups.dc.html` (Claude Design handoff, read in
full): a dark-only liquid-glass overlay system — translucent lenses with refracting
rim bands, specular top edges, concentric radii, ambient shadow instead of borders.
The mockups brand the app "Halo" — **the product is Verba**; treat every "Halo" string
in the design as placeholder copy. The ring mark itself carries no name and is kept.

Confirmed: **Tauri v2** · **Windows-only native** · **working loop before UI** ·
**`Ctrl+Shift+Space`** · **whisper.cpp**.

---

## Target machine

RTX 5070 Ti (16GB) · Ryzen 7 9700X · 31GB RAM · Rust, Node, Ollama all installed
(`qwen3.5:9b`, `gemma3:12b`, `gpt-oss:20b` already pulled).

This is over-provisioned for the task, which is good — it means `large-v3-turbo` and a
9B reformatter both fit in VRAM simultaneously, and the whole pipeline should land
well under the 400ms budget the design mockups advertise. **But develop against the
low end**, not this machine: assume an 8GB laptop with integrated graphics for
defaults, and let this box be the "everything on" configuration.

---

## Engine: whisper.cpp, not faster-whisper

Researched rather than assumed. Speed is close to a wash and faster-whisper may well
win on raw CPU throughput — CTranslate2 leans hard on oneDNN. It loses on the three
axes that actually matter for a laptop dictation tool.

**Memory.** faster-whisper's own README measures `small` INT8 at **1477 MB** on an
i7-12700K. whisper.cpp's `small` q5_1 is ~250 MB on disk and well under half that
resident. On an 8GB laptop already running a browser and an IDE, that gap decides
whether dictating causes a swap storm.

**Laptop GPUs.** This is the decisive one. faster-whisper requires **NVIDIA CUDA 12 +
cuBLAS + cuDNN 9** for any GPU acceleration — ROCm is unofficial forks, and Intel
iGPUs are unsupported entirely. Most laptops are Iris Xe or Radeon integrated, so
faster-whisper is CPU-only on the exact hardware you're targeting. whisper.cpp's
**Vulkan** backend is cross-vendor and runs on essentially any modern iGPU, with
OpenVINO as a second Intel path. Offloading to an idle iGPU instead of pegging CPU
cores *is* the "doesn't affect general performance" requirement.

**Deployment.** Tauri/Rust is confirmed, and CTranslate2 has no Rust binding — using
faster-whisper means shipping a Python sidecar, so ~200MB of runtime plus PyAV plus
process management and IPC. `whisper-rs` compiles into the binary. For something
people are meant to download instead of paying for SuperWhisper, that's a ~30MB
installer versus a ~300MB one.

Two follow-ons:

- **Responsiveness is a scheduling problem, not just a speed one.** Expose
  `n_threads` and cap it at physical cores − 2, and set the worker below-normal
  priority. A transcription that takes 800ms while the machine stays responsive beats
  one that takes 500ms and stutters your music. Make the cap a setting.
- whisper.cpp has **built-in Silero VAD**, so the plan's VAD step is a flag, not a
  dependency.

**Model ladder** (official ggml sizes — note the design's "142 MB" for its smallest
tier is exactly `base`, so the mockups were costed against real whisper.cpp numbers):

| tier | model | disk | notes |
|------|-------|------|-------|
| S | `base.en` q5_1 | ~60 MB | fast anywhere, weak on proper nouns |
| M | `small.en` q5_1 | ~250 MB | **default** — best accuracy/size for dictation |
| L | `large-v3-turbo` q5_0 | ~550 MB | 4-layer decoder, near-large accuracy at a fraction of the cost; viable on a Vulkan iGPU |

Ship with M. Keep the engine behind a trait so Parakeet via sherpa-onnx can be
swapped in later without touching the pipeline.

---

## Architecture

```
src-tauri/src/
  main.rs        state machine, Tauri wiring, tray
  hotkey.rs      WH_KEYBOARD_LL hook → press/release events
  audio.rs       cpal/WASAPI capture, ring buffer, 16k mono resample
  stt.rs         whisper-rs (Vulkan), model registry
  focus.rs       GetForegroundWindow → exe name + window title
  pipeline.rs    mode selection + the 3 formatting stages
  llm.rs         Ollama HTTP client (pluggable trait)
  inject.rs      clipboard paste w/ restore, SendInput fallback
  overlay.rs     window styles, backdrop capture
  store.rs       rusqlite + FTS5
  config.rs      TOML load/watch

src/
  overlay/       the pill + ribbon wave
  history/
  settings/
  shared/glass.css   material tokens
```

One state machine drives everything:

```
Idle ──hotkey down──► Listening ──hotkey up──► Transcribing ──► Formatting ──► Inject ──► Idle
                          │                                                        │
                          └────────────── Esc / discard ───────────────────────────┘
```

The frontend is a pure consumer: it receives `{phase, level, elapsed, text, mode}` and
renders. No logic in the webview. That keeps the overlay swappable and means the engine
can be driven headless in tests.

### The formatting pipeline

This is the part worth getting right — it's the reason the app exists.

```
audio
 → whisper (initial_prompt = custom vocabulary, biases decoding toward your jargon)
 → raw transcript
 → Stage 1  deterministic rules, ~0ms, always runs
      vocabulary replacement ("on ex" → "ONNX")
      spoken punctuation ("period" → ".", "new line" → "\n")
      filler removal (configurable: um, uh, like, you know)
      number/unit normalization
 → Stage 2  LLM pass, optional per-mode, ~300-800ms local
      system prompt = mode.instructions
      hard constraint: never invent facts, preserve names and numbers verbatim
 → Stage 3  output transform (trim, trailing space, case)
 → inject
```

Stage 1 exists separately from Stage 2 on purpose. It's instant and deterministic, so
the "Raw" mode (design 1d: *"No model pass. Punctuation only."*) is genuinely
zero-latency, and every other mode gets clean input to reformat.

**Mode selection, first match wins:**

1. Explicit user override (`Ctrl+Shift+1..5`, per design 1d)
2. First matching app rule
3. Default mode

```toml
default_mode = "raw"

[[modes]]
id = "email"
name = "Email"
description = "Clean prose, greeting + sign-off, no filler words."
llm = true
model = "qwen3.5:9b"
instructions = "Rewrite as a short email. Keep my voice. Never invent facts."
strip_fillers = true

[[rules]]                      # app-aware routing
mode = "code"
exe = ["Code.exe", "devenv.exe", "idea64.exe"]

[[rules]]
mode = "email"
exe = ["olk.exe", "OUTLOOK.EXE", "thunderbird.exe"]
title = "(?i)(compose|new message|^re:)"

[vocabulary]
terms = ["ONNX", "Kubernetes", "whisper.cpp"]
```

Modes are config, not code. Adding one is editing TOML. The Settings > Modes screen is
a TOML editor with a nicer face.

### Data

```sql
CREATE TABLE transcripts (
  id INTEGER PRIMARY KEY,
  created_at INTEGER NOT NULL,
  duration_ms INTEGER,
  raw TEXT NOT NULL,          -- pre-formatting
  output TEXT NOT NULL,       -- post-formatting
  mode_id TEXT,
  app_exe TEXT,
  app_title TEXT,
  word_count INTEGER,
  audio_path TEXT             -- NULL unless retention enabled
);
CREATE VIRTUAL TABLE transcripts_fts USING fts5(
  raw, output, content=transcripts, content_rowid=id);
```

Covers every column the history mockup (1e) displays: time, duration, word count, mode
badge, source app.

---

## Risks

### 1. `backdrop-filter` will not see the desktop — *decide this first*

The design's entire material is CSS `backdrop-filter`. In a transparent WebView2
window that filter samples **only the page's own compositing tree**. There is nothing
behind the element, so it blurs nothing. Every glass surface renders flat.

Three ways out, in order of fidelity:

- **(c) Capture-and-composite.** `BitBlt` the screen region behind the overlay into a
  bottom layer in the page, then every CSS filter in the design works verbatim —
  including the signature 8–12px rim band running its own `saturate(300%)`
  `brightness(1.15)` backdrop-filter, which the component sheet calls out as the
  defining material property. **No DWM backdrop can reproduce that band.** Static per
  invocation, which is fine for a 2–6 second interaction. ~40 lines of Rust. Call
  `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` so we don't capture
  ourselves.
- **(a) Tauri `WindowEffect::Acrylic`** on the HWND. Real desktop blur, but uniform
  across the whole window rect and the corner radius comes from
  `DWMWA_WINDOW_CORNER_PREFERENCE` (~8px, vs the design's 30px). Needs the window
  resized to hug the panel.
- **(b) `SetWindowRgn` + `CreateRoundRectRgn`** for the exact radius, but region
  clipping is aliased — visible stair-stepping on a 30px curve.

**Spike (c) before anything visual.** Fall back to (a)+(b) if capture proves janky.

The one piece of good news: the design's own choreography sidesteps most of this.
Frame 2a specifies idle = *nothing rendered at all*, and listening = *"glass still
fully transparent (ribbons float in air)"*. Only the extended transcript panel becomes
glass. So the hot path — pill and wave — needs no backdrop at all. The risk is confined
to one state.

For the ordinary windows (History, Settings, Onboarding) this is a non-issue:
`WindowEffect::Mica` gives exactly the *"Mica-style tint pulled from the wallpaper"*
the design asks for in 1e.

### 2. `RegisterHotKey` cannot do hold-to-talk

It fires on key-down only and never reports release. Hold-to-dictate needs
`SetWindowsHookEx(WH_KEYBOARD_LL)` on a dedicated thread with a message pump.

The callback must return in well under ~300ms or Windows silently unhooks you — so it
does nothing but push to a channel. All work happens elsewhere.

**`Ctrl+Shift+Space` collides with editors.** It's clear of anything Windows reserves,
but it's *Trigger Parameter Hints* in VS Code and *Parameter Info* in Visual Studio —
and hold-to-talk forces us to swallow the key, or the focused app receives a stream of
auto-repeats. So dictating in an IDE silently kills parameter hints, and VS Code is a
first-class target in the design's own app-routing table. Not a blocker, but make the
binding a setting from day one and pick a default that doesn't fight the editor
(`Ctrl+Alt+Space` and `Ctrl+Shift+;` are both unclaimed). Worth a second look once
you've dictated into VS Code a few times.

### 3. Focus must never move

If the overlay takes focus, the target app loses its caret and injection lands
nowhere. Overlay window needs `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, plus
`WS_EX_TRANSPARENT` toggled on while idle for click-through. Verify with a real text
field before building anything on top of it.

### 4. Blackwell + CUDA

`whisper-rs`'s `cuda` feature needs the CUDA toolkit at build time, and a 5070 Ti
(sm_120) needs 12.8+. **Use the `vulkan` feature instead** — no toolkit, works on any
GPU including AMD/Intel, and for whisper inference it's within a few percent of CUDA.
Also means anyone can build this without a 3GB toolkit install.

---

## Build order

Each milestone leaves one runnable check behind. No test framework.

### M0 · Spikes (throwaway, delete after)

- Transparent always-on-top no-activate WebView2 window. Type into Notepad with it
  visible — does the caret stay? Does `BitBlt` behind it produce a usable image?
- `whisper-rs` + Vulkan + `large-v3-turbo-q5_0`. Time a 10s clip end to end.

Answers to these determine M2's glass strategy and the default model. Don't skip.

### M1 · The loop, no UI  ← *first real milestone*

Hotkey → record → transcribe → paste. Tray icon only, nothing on screen.

- `hotkey.rs` — LL hook, press/release to a channel
- `audio.rs` — cpal WASAPI, ring buffer with **200ms pre-roll** so the first word
  isn't clipped when you start talking as you press
- `stt.rs` — whisper-rs, Vulkan, `small.en` q5_1 from `models/`, thread cap
- `inject.rs` — save clipboard → set text → `SendInput` Ctrl+V → restore after 200ms.
  Clipboard paste (not per-char `SendInput`) because it's the only thing reliable for
  long text. Unicode `SendInput` stays as a config fallback for apps that block paste.
- `focus.rs` — foreground exe + title, logged only for now

*Check:* `cargo run`, hold `Ctrl+Shift+Space` in Notepad, speak, release. Text
appears. Assert in `inject.rs` that the clipboard is byte-identical after restore.

This milestone decides whether the app is worth building. If latency or injection
reliability disappoint here, no amount of glass fixes it.

### M2 · Overlay + ribbon wave

Port the design. All values below are lifted directly from the mockup — no
reinterpretation needed.

**Wave** (frame 2b — the math is fully specified there):

```
f(x)   = env(x) · sin(2π·k·x + φ + t·s)
env(x) = sin(π·x)^1.4                    spindle taper to zero at both ends
amp    = a · (0.16 + 0.84·level) · 160 · 0.46
ribbon = filled area between +f(x) and −f(x)

7 ribbons (k, s, a, φ):
  (1.00,  0.85, 1.00, 0.0)   (1.45, -0.70, 0.88, 1.1)
  (2.05,  1.25, 0.72, 2.2)   (2.55, -1.10, 0.56, 0.6)
  (3.20,  0.60, 0.44, 3.4)   (0.72, -0.45, 0.52, 1.8)
  (1.15,  0.30, 0.16, 0.4)

colors in order  #6E7BFF #FF4E9A #A88BFF #FFB070 #59E7C4 #FF9AD5 #FFFFFF
all mix-blend-mode:screen inside an isolated group — crossings whiten, and that
whitening is what produces the lens shapes
bloom = the same group re-drawn through feGaussianBlur 11 at 0.78 opacity
viewBox 1000×160, preserveAspectRatio=none, 56 samples per path

level:  listening → 1 (×jitter) · processing → 0.34 (drift ×0.45) · idle → 0
smooth: level += (target − level) · min(1, dt·3.4)
jitter: 0.72 + 0.28·(0.5 + 0.5·sin(t·5.3)·sin(t·2.1+1.3))
```

Feed `level` from real RMS amplitude, not the mockup's synthetic jitter.

**Geometry** (component sheet 1j):

| state      | size    | radius | blur | notes                                    |
|------------|---------|--------|------|------------------------------------------|
| idle       | 168×36  | 18     | 30   | opacity .72, no bloom                    |
| listening  | 340×56  | 28     | 36   | displacement 26, bloom 44px pink 22%     |
| processing | 250×44  | 22     | 32   | displacement 14, indigo, linear 11–14s   |
| panel      | 660 w   | 30     | 40   | sat 180%, brightness .8                  |

**Transitions** (2a annotation):
expand `width→560, cubic-bezier(.2,1.04,.28,1) 780ms` · wave `scaleX .06→1,
cubic-bezier(.18,1.1,.26,1) 820ms` · extend `height 0→232,
cubic-bezier(.22,1,.36,1) 780ms`, rim fades in 700ms · word reveal cascade
`0 → .18 → .34 → .72 → 1`, .45s ease each.

**Interim text:** whisper isn't streaming-native. Rather than build chunked
pseudo-streaming, drive the design's existing word-reveal cascade from the *completed*
transcript over ~3.6s. The mockup's own script does exactly this — the opacity cascade
reads as live transcription whether or not it is. Real streaming is a later swap behind
the same interface, and it may never be worth it given hold-to-talk hands you the whole
buffer anyway.

*Check:* drive the state machine from a fake level signal, cycle idle→listen→panel,
confirm no dropped frames.

### M3 · Modes, app routing, LLM post-processing

`pipeline.rs` + `llm.rs` + the mode picker (design 1d). Ollama over HTTP behind a
trait so a cloud provider drops in later.

*Check:* table-driven assertions on Stage 1 — spoken punctuation, vocabulary
substitution, filler stripping — plus mode selection given a synthetic
`(exe, title)`. Pure functions, no I/O, no mocking.

### M4 · History, Settings, Onboarding

Three Mica windows (1e, 1f, 1g). SQLite + FTS5 behind the search field. Straight
design port; no new engine work.

---

## Deliberately not in v1

- Streaming/interim transcription — the reveal animation covers it (see M2)
- Speaker diarization, translation, multi-language switching
- Cloud STT — the trait exists, no implementation
- Audio retention — column exists, always NULL
- macOS/Linux — Win32 called directly, port is a rewrite of one module
- Auto-update, installer, telemetry

Add when you actually want them, not before.

---

## Verification

Per-milestone checks are listed above. End to end, once M3 lands:

1. `cargo tauri dev`
2. Open VS Code, hold `Ctrl+Shift+Space`, dictate a function description, release →
   text arrives formatted per the Code mode rules
3. Open Outlook, same phrase, same hotkey → arrives as prose with a greeting
4. Same phrase with `Ctrl+Shift+4` held (Raw) → arrives verbatim, punctuation only
5. `sqlite3 verba.db "select mode_id, app_exe, word_count from transcripts"` → three
   rows, three different modes

Step 3 vs step 4 is the whole product in two keystrokes. If those differ correctly,
the thing works.

Separately, on the lowest-spec machine you can find: dictate 20 seconds while playing
music. If audio stutters, the thread cap is too high — that check matters more than
any transcription benchmark, because it's the actual product requirement.
