# Verba

Local-first speech-to-text for Windows 11. Hold `Ctrl+Shift+Space`, speak, release —
text lands at your caret. Everything runs on-device.

Status: **M2** — overlay and ribbon visualiser. See [PLAN.md](PLAN.md).

## Run it

```bash
cd src-tauri && cargo run --release
```

Hold `Ctrl+Shift+Space`, say something, let go. The text pastes wherever your caret
is. `Ctrl+C` to quit.

## Build prerequisites

- **Rust** (MSVC toolchain)
- **CMake** — whisper.cpp is compiled from source
- **VS Build Tools** with the C++ workload
- **libclang** — `whisper-rs-sys` generates its FFI bindings with bindgen

The libclang requirement is the only awkward one. Rather than install the full ~2.5GB
LLVM toolchain for a single DLL:

```bash
pip install libclang
```

then copy `<site-packages>/clang/native/libclang.dll` into `src-tauri/.tools/`.
`src-tauri/.cargo/config.toml` points `LIBCLANG_PATH` there with a repo-relative path.

Note the crate's bundled `bindings.rs` is *not* a way around this — it was generated
on Linux and carries glibc struct layouts that fail a const-eval size assertion under
MSVC.

## Model

Default is `small.en` q5_1 (181 MB), expected at `models/`:

```bash
curl -L -o models/ggml-small.en-q5_1.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en-q5_1.bin
```

Override with `VERBA_MODEL=/path/to/model.bin`. Other tiers are in
[PLAN.md](PLAN.md#engine-whispercpp-not-faster-whisper).

## Tests

```bash
cd src-tauri && cargo test
```

Covers the ring buffer's absolute indexing (marks must survive rotation), resampling,
and keyboard event construction. The injection tests check `events_for` rather than
calling `insert` — that would type into whatever window had focus while the suite ran.

To exercise injection for real, with no model load and no microphone:

```bash
cd src-tauri && .\target\release\verba.exe --inject-test "hello"
```

## Notes on the overlay

The window carries `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW`. The
first is essential: if the overlay ever took focus, the caret would leave the target
app and there would be nowhere to insert.

Text is inserted by synthesizing `KEYEVENTF_UNICODE` events, not by staging on the
clipboard and sending Ctrl+V. The clipboard route has three independent failure modes
and destroys whatever the user had copied.

## Notes

- CPU-only for now. Vulkan offload is a `whisper-rs` feature flag, added once the
  loop is proven.
- Thread count is capped at physical cores − 2 so dictating doesn't stutter the rest
  of the machine.
- Audio capture runs continuously into a 120s ring buffer, so pressing the hotkey can
  reach 200ms *backwards* and catch the word you started as you pressed.
