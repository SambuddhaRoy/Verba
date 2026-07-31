//! Model downloads with progress.
//!
//! Streams to a `.part` file and renames on success, so an interrupted
//! download is never mistaken for an installed model.

use std::io::{Read, Write};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

pub const EVENT: &str = "verba:download";

#[derive(Clone, serde::Serialize)]
pub struct Progress {
    pub file: String,
    pub received: u64,
    pub total: u64,
    pub done: bool,
    /// "downloading" or "extracting". Without it the frontend could only infer
    /// state from the byte counts, and the post-download notification — which
    /// carries no counts — rendered as an empty bar reading "0 / 0 MB", which
    /// looks like the transfer restarted.
    pub stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Core transfer. Reports `(received, total)` as it goes; `total` is 0 when the
/// server withholds a content length.
///
/// Separate from the Tauri layer so it can run headless — the CLI path uses it
/// directly, which is also how this code gets exercised without clicking a
/// button.
pub fn fetch_with<F: FnMut(u64, u64)>(
    file: &str,
    url: &str,
    mut on_progress: F,
) -> Result<PathBuf, String> {
    let dir = crate::config::models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let target = dir.join(file);
    let part = dir.join(format!("{file}.part"));

    let result = (|| -> Result<(), String> {
        let resp = ureq::get(url).call().map_err(|e| e.to_string())?;

        let total: u64 = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let mut reader = resp.into_body().into_reader();
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;

        // 256KB: large enough that a 1.5GB model is not a million syscalls,
        // small enough that progress still looks continuous.
        let mut buf = vec![0u8; 256 * 1024];
        let mut received: u64 = 0;
        let mut last_report = 0u64;

        loop {
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            received += n as u64;

            // Report at most every whole percent, or the event channel carries
            // thousands of messages nobody can tell apart.
            let step = (total / 100).max(1 << 20);
            if received - last_report >= step {
                last_report = received;
                on_progress(received, total);
            }
        }
        out.flush().map_err(|e| e.to_string())?;
        drop(out);

        if total > 0 && received < total {
            return Err(format!("truncated: got {received} of {total} bytes"));
        }
        std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
        Ok(())
    })();

    match result {
        Ok(()) => Ok(target),
        Err(e) => {
            // Never leave a partial behind to be mistaken for a real model.
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Tauri-facing wrapper: same transfer, progress emitted to the settings window.
///
/// `id` is what the UI keys its row on; `saved_as` is the file on disk, which
/// differs when the download is an archive that gets unpacked. `finish` runs
/// after a successful transfer and is where extraction happens — a failure
/// there is reported as a failed download, because a half-unpacked model is no
/// more usable than a half-downloaded one.
pub fn fetch_named<F>(app: &AppHandle, id: &str, saved_as: &str, url: &str, finish: F)
where
    F: FnOnce(&std::path::Path) -> anyhow::Result<()>,
{
    let emit = |p: Progress| {
        if let Err(e) = app.emit(EVENT, p) {
            crate::log!("download progress emit failed: {e}");
        }
    };

    let outcome = fetch_with(saved_as, url, |received, total| {
        emit(Progress {
            file: id.into(), received, total, done: false,
            stage: "downloading", error: None,
        });
    })
    .and_then(|path| {
        // Unpacking a few hundred megabytes is not instant; say so rather than
        // leaving the bar sitting full. The bar holds at 100% and the label
        // changes, instead of resetting to zero.
        emit(Progress {
            file: id.into(), received: 1, total: 1, done: false,
            stage: "extracting", error: None,
        });
        finish(&path).map_err(|e| e.to_string()).map(|()| path)
    });

    match outcome {
        Ok(path) => {
            crate::log!("installed {id} -> {}", path.display());
            emit(Progress {
                file: id.into(), received: 1, total: 1, done: true,
                stage: "done", error: None,
            });
        }
        Err(e) => {
            crate::log!("download failed for {id}: {e}");
            emit(Progress {
                file: id.into(), received: 0, total: 0, done: true,
                stage: "failed", error: Some(e),
            });
        }
    }
}
