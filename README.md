# Verba

Local-first speech-to-text for Windows 11. Hold `Ctrl+Shift+Space`, speak, let go —
your words land at the caret in whatever app you were using. Nothing is uploaded, there
is no account, and there is no per-minute cost.

It is a free alternative to the paid dictation tools, built around the idea that the
output should already be shaped for where it is going: an email in Outlook, bullets in
Obsidian, a comment in your editor.

---

## Install

1. Download `Verba.exe` from the [latest release](../../releases/latest).
2. Run it. It lives in the system tray; there is no installer and nothing is written
   outside `%LOCALAPPDATA%\Verba`.
3. On first launch a short setup appears — pick a microphone, download one speech
   model, and test it.

**Windows will warn you the first time.** The executable is not code-signed (a
certificate costs several hundred dollars a year), so SmartScreen shows
*"Windows protected your PC"*. Choose **More info → Run anyway**. If you would rather
not trust a binary from a stranger, build it yourself — see below.

### Updates

Verba checks GitHub for a new release shortly after launch and every six hours
after that. When one appears it downloads it in the background, verifies it against
the SHA-256 GitHub publishes for the asset, and swaps it in — but only once you have
gone two minutes without dictating, so an update can never interrupt you mid-sentence.
Turn it off under **Settings → About → Update automatically**, or check by hand from
the same panel.

A caveat worth stating plainly: the checksum and the download come from the same
place, so this protects against a corrupted or truncated transfer, not against a
compromised GitHub account. Real protection would need a signature made with a key
that never touches GitHub, which this project does not have yet.

### Uninstall

Quit from the tray, delete `Verba.exe`, and delete `%LOCALAPPDATA%\Verba` if you want
the models and config gone too.

---

## What it does

**Dictate anywhere.** A low-level keyboard hook watches for the hotkey and swallows it
while held, so the focused app never sees it. Audio is captured continuously into a
120-second ring buffer, which means pressing the hotkey can reach ~200 ms *backwards*
and catch the word you started saying as you pressed.

**Formats for the app you are in.** Verba reads the foreground window's executable and
title and picks a mode. Out of the box: `raw`, `code`, `email`, `chat` and `notes`,
routed across about fifty applications. Every rule, mode and prompt is editable.

**Cleans up speech.** Spoken punctuation ("new paragraph", "open bracket"), filler
words, casing and a custom vocabulary list are applied deterministically — no model
involved, so it cannot hallucinate.

**Learns what you keep fixing.** Correct a dictation in **Settings → Vocabulary**
and Verba diffs it against what the recogniser produced. A term you fix twice is
suggested to the model as bias; one you fix three times is corrected outright. Off
until you turn it on, and the history is a plain-text file you can read or delete.

The automatic-rewrite path is deliberately guarded. A correction is only applied
without the model's help when what the recogniser produced is *not* ordinary
English — "cuber netties" is safe to rewrite, "there" is not, however many times
you fixed it. A homophone fix is about one sentence, and applying it everywhere
would silently corrupt words you did say.

**Domain packs.** Vocabulary plus formatting rules for a field, several enabled at
once. Ships with Code, Medical and Legal. The Code pack turns "print open paren
hello close paren" into `print(hello)`; the Medical pack knows "bee pee" is `BP`.
Add your own as a JSON file in `%APPDATA%\Verba\packs`.

**Optionally rewrites with a local LLM.** If [Ollama](https://ollama.com) is installed,
a small model can turn dictated speech into properly written prose for the target app.
Verba starts the server itself when needed and will only offer models of 4B parameters
or smaller, since this runs between you finishing a sentence and the text appearing.
A fidelity check rejects any rewrite that drops more than 60% of the words or loses a
number that was in the original, falling back to the cleaned transcript.

**Follows Windows.** The accent colour and light/dark theme are read from the same
settings Windows uses for its own chrome, and re-read while running — so changing
your accent, or a wallpaper that Windows derives an accent from, retints the app
within a second or two rather than at the next restart. Light mode is a full
variant, not an inverted filter. The overlay stays a dark lens in both, because it
floats over whatever you are working in.

**Three overlay treatments.** An interference-pattern ribbon visualiser, a glow that
deforms with the spectrum, and an ultra-minimal recorder. All three are audio-reactive
and tinted with your Windows accent colour.

---

## Requirements

|                | |
|----------------|--|
| **OS**         | Windows 11 (Windows 10 likely works but is untested) |
| **RAM**        | 4 GB free for the small models, 8 GB+ for the large ones |
| **GPU**        | Optional. A Vulkan-capable GPU is used automatically if present |
| **Ollama**     | Optional, only for the LLM rewrite pass |

Verba detects your hardware on first run and recommends a model to match.

---

## Speech models

Models are downloaded in-app and stored in `%LOCALAPPDATA%\Verba\models`.

| Model | Size | Notes |
|---|---:|---|
| Whisper Tiny (English) | 31 MB | Fastest, roughest. Fine for short commands |
| Whisper Base (English) | 57 MB | |
| Whisper Small (English) | 181 MB | The sensible default on modest hardware |
| Whisper Large v3 Turbo | 547 MB | Near-large accuracy, multilingual |
| Parakeet TDT 0.6B v2 | 460 MB | Tops several English accuracy leaderboards |

Every model carries an **accuracy** and a **speed** bar so the trade-off between them
is visible rather than something you have to already know. The two are deliberately
kept separate — a single blended score would hide exactly the decision you are making.
Speed is rated for *your* machine: the same model is quick with GPU offload and slow
without it, so a model that would have to spill to CPU says so under its bar.

Verba picks a recommendation from those same numbers, weighted slightly towards
accuracy, and skips anything that will not fit in memory. The reason names your actual
hardware, so it reads as a decision rather than a default.

Three engines are supported. **whisper.cpp** is compiled into the binary and needs
nothing extra. **Parakeet** (via sherpa-onnx) and **faster-whisper** run as Python
sidecars, and Verba installs their dependencies on demand when you pick one.

Those two need Python 3.9 or newer. Verba checks for it before you commit to an
engine, and offers to install it via winget. Note that Windows puts placeholder
`python.exe` and `python3.exe` files on your PATH which only open the Microsoft
Store — so typing `python` in a terminal appears to do something even when nothing
is installed. Verba ignores those and looks for a real interpreter; `--python`
reports what it found.

For a rough sense of scale, on one laptop (RTX-class GPU, Vulkan): whisper.cpp
`small.en` transcribed a short utterance in ~300 ms, and Parakeet TDT 110M on CPU did
the same in ~90 ms. Treat those as one data point, not a benchmark.

---

## Building from source

```bash
git clone https://github.com/SambuddhaRoy/Verba
cd Verba
powershell -NoProfile -File tools/build.ps1
```

Output lands in `dist/Verba.exe`.

**Prerequisites**

- Rust, MSVC toolchain
- CMake and VS Build Tools with the C++ workload — whisper.cpp is compiled from source
- The Vulkan SDK, for GPU offload. Without it, build with `--no-default-features`
- `libclang`, because `whisper-rs-sys` generates its FFI bindings with bindgen

The libclang requirement is the awkward one. Rather than install the whole ~2.5 GB LLVM
toolchain for a single DLL:

```bash
pip install libclang
```

then copy `<site-packages>/clang/native/libclang.dll` into `src-tauri/.tools/`.
`src-tauri/.cargo/config.toml` points `LIBCLANG_PATH` at that folder with a
repo-relative path, so the build stays portable.

The crate's bundled `bindings.rs` is not a shortcut around this — it was generated on
Linux and carries glibc struct layouts that fail a const-eval size assertion under
MSVC.

> Cargo discovers `.cargo/config.toml` from the **working directory**, not from
> `--manifest-path`. Run cargo from inside `src-tauri/`, or `LIBCLANG_PATH` will not be
> set and the build will fail in `whisper-rs-sys`.

### Tests

```bash
cd src-tauri && cargo test
```

Covers the ring buffer's absolute indexing, resampling, keyboard event construction,
the formatting rules, the Ollama catalogue and hardware tiers, and the guard that stops
a dictation typing into Verba's own window. Injection is tested through `events_for`
rather than `insert`, which would type into whatever window had focus while the suite
ran.

### Diagnostics

The binary carries its own instruments. Each exists because something was once wrong
that could not be diagnosed by reading the code.

| Command | What it answers |
|---|---|
| `--state` | The exact payload the settings window renders from |
| `--meters [secs]` | Live spectrum from the real microphone |
| `--transcribe <wav>` | Engine timing without the hotkey or overlay |
| `--format <text> <exe>` | Mode routing and both formatting passes, no microphone |
| `--accent` | The Windows accent colour and theme as Verba resolves them |
| `--python` | Which interpreter the sidecar engines will use, and why |
| `--capture-test` | Whether the desktop capture behind the overlay works |
| `--inject-test <text>` | Text insertion with no model and no microphone |
| `--overlay-test [visual]` | Drive the overlay through its states |
| `--onboard` | Replay the first-run flow |
| `--fix "<heard>" "<meant>"` | Record a correction without dictating it |
| `--learned` | Every learned term, and what each engine will do with it |
| `--check-update` | What the background watcher sees, without waiting for it |
| `--self-update` | Run the whole cycle now: check, download, verify, swap, relaunch |

---

## Status

Verba is early. It works and it is used daily, but the version number is honest.

Known gaps: Parakeet runs in offline mode rather than truly streaming; there is no
dictation history; per-mode override hotkeys are not implemented; Windows 10 is
untested.

**Recogniser bias is whisper-only.** Learned terms and pack vocabulary are fed to
whisper through `initial_prompt`. sherpa-onnx has an equivalent — hotwords — but it
encodes them against the model's own token table, and Parakeet ships a 1025-piece
BPE vocabulary with no sentencepiece model to split new words with, so every hotword
is rejected as unencodable. On Parakeet, learned terms still take effect as
deterministic rewrites; the model itself is not steered. `--learned` says which
applies to the engine you are on.

Windows only for now. The platform layer is Win32 throughout, and a Linux or
Android port is a separate piece of work rather than a flag.

---

## Licence

Verba is free software under the [GNU GPL v3](LICENSE). You may use, study, share and
modify it. If you distribute a modified version, it must also be GPL v3 and its source
must be available.

Third-party components keep their own licences — whisper.cpp (MIT), Tauri (MIT/Apache
2.0), sherpa-onnx (Apache 2.0). Speech models each carry their own terms, shown next to
them in the models list; Parakeet in particular is CC-BY-4.0 from NVIDIA.

Copyright (C) 2026 Sambuddha Roy.
