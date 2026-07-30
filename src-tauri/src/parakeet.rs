//! Parakeet / sherpa-onnx backend, driven as a Python sidecar.
//!
//! sherpa-onnx ships prebuilt wheels with the native library and ONNX Runtime
//! bundled, so this needs no CMake and no vendored C++ — unlike the `sherpa-rs`
//! route, which would rebuild sherpa-onnx from source through the same
//! toolchain that already cost a MAX_PATH fight.
//!
//! Worth knowing: the Parakeet TDT models here are *offline*. sherpa-onnx also
//! publishes genuinely streaming Parakeet builds, which are not wired up yet.
//! What Parakeet buys today is raw speed — roughly 90x realtime on CPU for the
//! 110m build against about 25x for whisper small.

use anyhow::{anyhow, bail, Result};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const SIDECAR: &str = include_str!("../resources/parakeet_sidecar.py");

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn engine_dir() -> PathBuf {
    crate::config::dir().join("engines").join("parakeet")
}

fn venv_python() -> PathBuf {
    engine_dir().join("Scripts").join("python.exe")
}

pub fn is_installed() -> bool {
    venv_python().is_file()
}

pub fn install<F: FnMut(&str)>(mut report: F) -> Result<()> {
    let dir = engine_dir();
    std::fs::create_dir_all(&dir)?;

    if !venv_python().is_file() {
        report("creating Python environment…");
        let out = Command::new("python")
            .args(["-m", "venv", &dir.to_string_lossy()])
            .output()
            .map_err(|e| anyhow!("python not found on PATH: {e}"))?;
        if !out.status.success() {
            bail!("venv failed: {}", String::from_utf8_lossy(&out.stderr));
        }
    }

    report("installing sherpa-onnx…");
    let out = Command::new(venv_python())
        .args(["-m", "pip", "install", "--disable-pip-version-check", "sherpa-onnx", "numpy"])
        .output()?;
    if !out.status.success() {
        bail!("pip install failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    report("verifying…");
    let out = Command::new(venv_python())
        .args(["-c", "import sherpa_onnx, numpy; print(sherpa_onnx.__version__)"])
        .output()?;
    if !out.status.success() {
        bail!("import check failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    report(&format!(
        "ready (sherpa-onnx {})",
        String::from_utf8_lossy(&out.stdout).trim()
    ));
    Ok(())
}

/// Unpack a downloaded `.tar.bz2` into the models directory.
///
/// Done through the engine's own Python rather than a Rust tar/bzip2 pair:
/// the interpreter is already a hard requirement here, `tarfile` handles bz2
/// natively, and it is two dependencies not added.
pub fn extract(archive: &std::path::Path) -> Result<()> {
    if !is_installed() {
        bail!("Parakeet engine is not installed yet");
    }
    let dest = crate::config::models_dir();
    let code = format!(
        // filter='data' refuses absolute paths and parent traversal in member
        // names, so a hostile archive cannot write outside the destination.
        "import tarfile; tarfile.open(r'{}').extractall(r'{}', filter='data')",
        archive.display(),
        dest.display()
    );
    let out = Command::new(venv_python()).args(["-c", &code]).output()?;
    if !out.status.success() {
        bail!("extract failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let _ = std::fs::remove_file(archive);
    Ok(())
}

pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    scratch: PathBuf,
    dir: String,
    threads: i32,
}

impl Sidecar {
    pub fn new(model: &str, threads: Option<i32>) -> Result<Self> {
        if !is_installed() {
            bail!("Parakeet engine is not installed yet");
        }
        let dir = crate::config::models_dir().join(model);
        if !dir.join("tokens.txt").is_file() {
            bail!("{model} is not downloaded yet");
        }

        let script = engine_dir().join("sidecar.py");
        std::fs::write(&script, SIDECAR)?;

        let mut cmd = Command::new(venv_python());
        cmd.arg("-u")
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?);

        let mut s = Self {
            child,
            stdin,
            stdout,
            scratch: crate::config::dir().join("parakeet.f32"),
            dir: dir.to_string_lossy().into_owned(),
            threads: threads.filter(|t| *t > 0).unwrap_or_else(|| {
                num_cpus::get_physical().saturating_sub(2).max(1) as i32
            }),
        };

        let hello = s.read_reply()?;
        if !hello.ok {
            bail!("sidecar failed to start: {}", hello.error.unwrap_or_default());
        }

        // Build the recogniser now rather than on the first transcription.
        // Left lazy, the ONNX session setup lands inside the first decode —
        // which makes "preload model" a lie and puts a quarter-second stall on
        // the first thing the user dictates.
        let req = serde_json::json!({
            "op": "load", "dir": s.dir, "threads": s.threads, "provider": "cpu",
        });
        writeln!(s.stdin, "{req}")?;
        s.stdin.flush()?;
        let loaded = s.read_reply()?;
        if !loaded.ok {
            bail!("model load failed: {}", loaded.error.unwrap_or_default());
        }

        crate::log!("parakeet sidecar up ({model})");
        Ok(s)
    }

    fn read_reply(&mut self) -> Result<Reply> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            bail!("sidecar exited");
        }
        Ok(serde_json::from_str(&line)?)
    }

    pub fn transcribe(&mut self, pcm: &[f32], _quick: bool) -> Result<String> {
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&self.scratch, &bytes)?;

        let req = serde_json::json!({
            "op": "transcribe",
            "dir": self.dir,
            "pcm": self.scratch.to_string_lossy(),
            "threads": self.threads,
            "provider": "cpu",
        });
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;

        let reply = self.read_reply()?;
        if !reply.ok {
            bail!("{}", reply.error.unwrap_or_else(|| "unknown error".into()));
        }
        if let (Some(r), Some(d)) = (reply.read_ms, reply.decode_ms) {
            crate::log!("  sidecar: read {r}ms, decode {d}ms");
        }
        Ok(reply.text.unwrap_or_default())
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, r#"{{"op":"quit"}}"#);
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.scratch);
    }
}

#[derive(serde::Deserialize)]
struct Reply {
    ok: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    read_ms: Option<f32>,
    #[serde(default)]
    decode_ms: Option<f32>,
}
