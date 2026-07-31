//! faster-whisper backend, driven as a Python sidecar.
//!
//! CTranslate2 has no Rust bindings, so this is the only way to reach it. The
//! sidecar is embedded in the binary and written out on first use, so there is
//! no loose script to ship or keep in sync.
//!
//! Worth knowing what this costs relative to whisper.cpp: it needs a Python
//! runtime, and its GPU path is CUDA-only — no Vulkan, no Intel graphics. On a
//! machine where whisper.cpp reaches the GPU, this will usually be slower.

use anyhow::{anyhow, bail, Result};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const SIDECAR: &str = include_str!("../resources/faster_whisper_sidecar.py");

/// Hide the console window a child process would otherwise flash up. Verba is
/// a GUI app; a black box appearing on every dictation is not acceptable.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn engine_dir() -> PathBuf {
    crate::config::dir().join("engines").join("faster-whisper")
}

fn venv_python() -> PathBuf {
    engine_dir().join("Scripts").join("python.exe")
}

pub fn is_installed() -> bool {
    venv_python().is_file()
}

/// Create the virtual environment and install faster-whisper into it.
///
/// `report` receives human-readable progress lines. Blocking, and slow the
/// first time: callers run it off the UI thread.
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

    report("installing faster-whisper (this takes a few minutes)…");
    let out = Command::new(venv_python())
        .args(["-m", "pip", "install", "--disable-pip-version-check", "faster-whisper"])
        .output()?;
    if !out.status.success() {
        bail!("pip install failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    report("verifying…");
    let out = Command::new(venv_python())
        .args(["-c", "import faster_whisper, ctranslate2; print(ctranslate2.__version__)"])
        .output()?;
    if !out.status.success() {
        bail!("import check failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    report(&format!(
        "ready (ctranslate2 {})",
        String::from_utf8_lossy(&out.stdout).trim()
    ));
    Ok(())
}

pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    scratch: PathBuf,
    model: String,
    device: &'static str,
}

impl Sidecar {
    pub fn new(model: &str) -> Result<Self> {
        if !is_installed() {
            bail!("faster-whisper is not installed yet");
        }
        let script = engine_dir().join("sidecar.py");
        // Rewrite every launch: the embedded copy is the source of truth, so an
        // upgraded Verba never runs a stale script left by an older one.
        std::fs::write(&script, SIDECAR)?;

        let mut cmd = Command::new(venv_python());
        cmd.arg("-u") // unbuffered, or replies sit in the pipe
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
        // Tie it to the process job so a hard exit cannot orphan it.
        crate::childguard::adopt(child.id());
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?);

        // CUDA when it is there, CPU otherwise. CTranslate2 has no Vulkan path,
        // so on an AMD or Intel GPU this is CPU regardless.
        let device = if cuda_present() { "cuda" } else { "cpu" };

        let mut s = Self {
            child,
            stdin,
            stdout,
            scratch: crate::config::dir().join("scratch.f32"),
            model: model.into(),
            device,
        };

        // The sidecar announces itself once imports succeed; a failure here is
        // a broken environment, and better surfaced now than mid-dictation.
        let hello = s.read_reply()?;
        if !hello.ok {
            bail!("sidecar failed to start: {}", hello.error.unwrap_or_default());
        }

        // Load now, not on the first transcription. Left lazy this also
        // downloads the weights mid-dictation the first time a model is used.
        let req = serde_json::json!({
            "op": "load",
            "model": s.model,
            "device": s.device,
            "compute": if s.device == "cuda" { "float16" } else { "int8" },
        });
        writeln!(s.stdin, "{req}")?;
        s.stdin.flush()?;
        let loaded = s.read_reply()?;
        if !loaded.ok {
            bail!("model load failed: {}", loaded.error.unwrap_or_default());
        }

        crate::log!("faster-whisper sidecar up ({device})");
        Ok(s)
    }

    fn read_reply(&mut self) -> Result<Reply> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            bail!("sidecar exited");
        }
        Ok(serde_json::from_str(&line)?)
    }

    pub fn transcribe(&mut self, pcm: &[f32], quick: bool) -> Result<String> {
        // Raw f32 to a scratch file. Inline base64 would triple the payload and
        // put megabytes through a line-delimited pipe.
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&self.scratch, &bytes)?;

        let req = serde_json::json!({
            "op": "transcribe",
            "model": self.model,
            "device": self.device,
            "compute": if self.device == "cuda" { "float16" } else { "int8" },
            "pcm": self.scratch.to_string_lossy(),
            "language": crate::config::load().language,
            "quick": quick,
        });
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;

        let reply = self.read_reply()?;
        if !reply.ok {
            bail!("{}", reply.error.unwrap_or_else(|| "unknown error".into()));
        }
        Ok(reply.text.unwrap_or_default())
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, r#"{{"op":"quit"}}"#);
        let _ = self.stdin.flush();
        // Do not wait indefinitely on a wedged interpreter.
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
}

/// CTranslate2 loads CUDA at runtime; presence of the driver library is the
/// cheapest reliable signal without pulling in a CUDA dependency ourselves.
fn cuda_present() -> bool {
    Path::new(r"C:\Windows\System32\nvcuda.dll").is_file()
}
