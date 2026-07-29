# Verba

Local-first speech-to-text for Windows 11. Hold `Ctrl+Shift+Space`, speak, release —
text lands at your caret. Everything runs on-device.

Status: **M1** — the working loop. No UI yet. See [PLAN.md](PLAN.md).

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
and clipboard save/restore. The clipboard test deliberately does not call `insert` —
that would synthesize a real `Ctrl+V` into whatever window has focus.

## Notes

- CPU-only for now. Vulkan offload is a `whisper-rs` feature flag, added once the
  loop is proven.
- Thread count is capped at physical cores − 2 so dictating doesn't stutter the rest
  of the machine.
- Audio capture runs continuously into a 120s ring buffer, so pressing the hotkey can
  reach 200ms *backwards* and catch the word you started as you pressed.
