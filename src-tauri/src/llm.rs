//! The optional rewrite pass, via a local Ollama server.
//!
//! Ollama rather than a bundled llama.cpp: it is already the way most people
//! have local models on Windows, it manages the weights itself, and it means
//! Verba does not ship a second inference engine alongside whisper.cpp.
//!
//! Everything here is best-effort. The caller inserts the cleaned transcript
//! when this fails, because losing a dictation to a stopped background service
//! is far worse than inserting slightly less polished words.

use anyhow::{anyhow, Result};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::Config;

/// This sits directly between the user finishing a sentence and their text
/// appearing, so it is budgeted as a UI delay rather than a network one. Eight
/// seconds is generous for a 9B model rewriting a paragraph on a warm GPU;
/// past that the cleaned transcript is the better answer. Measured at 20s the
/// wait was intolerable — a half-started Ollama that accepts connections but
/// never replies held every insertion for the full duration.
const TIMEOUT: Duration = Duration::from_secs(8);

#[derive(serde::Deserialize)]
struct GenerateReply {
    #[serde(default)]
    response: String,
}

/// How long to stop trying after the server refuses a connection.
///
/// A refused connection took two seconds to surface in testing, and that is
/// paid on *every* dictation while Ollama is not running — the fallback text
/// is fine, but a silent two-second delay before every insertion is not. One
/// slow failure per minute is a fair price for noticing when it comes back.
const DOWN_FOR: Duration = Duration::from_secs(60);

static UNREACHABLE_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

fn recently_unreachable() -> bool {
    matches!(*UNREACHABLE_SINCE.lock().unwrap_or_else(|e| e.into_inner()),
             Some(t) if t.elapsed() < DOWN_FOR)
}

fn note_reachability(ok: bool) {
    let mut guard = UNREACHABLE_SINCE.lock().unwrap_or_else(|e| e.into_inner());
    *guard = if ok { None } else { Some(Instant::now()) };
}

pub fn rewrite(cfg: &Config, instructions: &str, text: &str) -> Result<String> {
    // No model chosen yet — the default state of a fresh install. Checked
    // before ensure_running() so a first run never starts a server it has
    // nothing to ask.
    if cfg.llm_model.trim().is_empty() {
        return Err(anyhow!("no rewrite model selected"));
    }
    if recently_unreachable() {
        return Err(anyhow!("skipped: {} was unreachable moments ago", cfg.llm_url));
    }
    // Start the server if it is installed but idle. The negative cache above
    // keeps this from being attempted on every dictation when Ollama is not
    // installed at all.
    if let Err(e) = crate::ollama::ensure_running(cfg) {
        note_reachability(false);
        return Err(e);
    }
    let url = format!("{}/api/generate", cfg.llm_url.trim_end_matches('/'));

    let body = serde_json::json!({
        "model": cfg.llm_model,
        "system": format!(
            "{instructions}\n\nYou are rewriting dictated speech. Output only the \
             rewritten text with no preamble, no explanation and no code fences. \
             Never add facts, names or numbers that are not in the input."
        ),
        "prompt": text,
        "stream": false,
        "options": {
            // Low temperature: this is a reformatting task, and sampling
            // variety here shows up as invented wording.
            "temperature": 0.2,
            "top_p": 0.9,
        },
        // Reasoning models otherwise emit their chain of thought into the
        // response field, which would be typed into the user's document.
        "think": false,
    });

    let t0 = Instant::now();
    let sent = crate::net::post(&url, "rewrite a dictation with the local LLM")
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .send_json(&body);
    // Separating the round-trip from the total is what distinguishes a slow
    // model from a cold one — a first request after startup pays the weight
    // load and can eat the whole timeout, which is why preload() exists.
    crate::log!("  llm: round-trip {}ms", t0.elapsed().as_millis());

    // Only a transport failure means "down"; a model that errors is a
    // different problem and should not stop us trying again next time.
    note_reachability(sent.is_ok());

    let reply: GenerateReply = sent
        .map_err(|e| anyhow!("{} unreachable at {url}: {e}", cfg.llm_model))?
        .body_mut()
        .read_json()
        .map_err(|e| anyhow!("unreadable reply: {e}"))?;

    let text = strip_thinking(&reply.response);
    if text.trim().is_empty() {
        return Err(anyhow!("empty response"));
    }
    Ok(text)
}

/// Drop a `<think>` block if one leaks through despite `think: false`, and
/// peel a code fence if the model wrapped its answer in one.
fn strip_thinking(s: &str) -> String {
    let mut out = s;
    if let Some(end) = out.find("</think>") {
        out = &out[end + "</think>".len()..];
    }
    let t = out.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // Drop the language tag on the opening fence, then the closing fence.
        let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or(rest);
        if let Some(body) = rest.rsplit_once("```") {
            return body.0.trim().to_string();
        }
    }
    t.to_string()
}

/// Models the local Ollama server currently has, for the settings picker.
/// An unreachable server is not an error here — it just means no choices.
pub fn installed_models(cfg: &Config) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Tags {
        #[serde(default)]
        models: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        #[serde(default)]
        name: String,
    }

    let url = format!("{}/api/tags", cfg.llm_url.trim_end_matches('/'));
    let Ok(mut resp) = crate::net::get(&url, "list local Ollama models")
        .config()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .call()
    else {
        return Vec::new();
    };
    note_reachability(true);
    resp.body_mut()
        .read_json::<Tags>()
        .map(|t| t.models.into_iter().map(|m| m.name).filter(|n| !n.is_empty()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::strip_thinking;

    /// A reasoning model's chain of thought must never reach the user's
    /// document, and a fenced answer must not arrive with its backticks.
    #[test]
    fn thinking_and_fences_are_removed() {
        assert_eq!(strip_thinking("<think>hmm, tone?</think>Hello there."), "Hello there.");
        assert_eq!(strip_thinking("```\nHello there.\n```"), "Hello there.");
        assert_eq!(strip_thinking("```text\nHello there.\n```"), "Hello there.");
        assert_eq!(strip_thinking("  Hello there.  "), "Hello there.");
    }

    /// The cache is what keeps a stopped Ollama from costing two seconds on
    /// every dictation, so the transitions need to be right in both directions.
    #[test]
    fn unreachable_is_remembered_and_cleared() {
        use super::{note_reachability, recently_unreachable};
        note_reachability(true);
        assert!(!recently_unreachable(), "a reachable server must not be cached as down");

        note_reachability(false);
        assert!(recently_unreachable(), "a refused connection must suppress the next attempt");

        // Coming back must clear it immediately rather than waiting out the
        // window — installed_models() calls this when the settings window opens.
        note_reachability(true);
        assert!(!recently_unreachable(), "recovery must be picked up at once");
    }

    #[test]
    fn unfenced_backticks_survive() {
        // Inline code in the middle of a sentence is not a fence.
        assert_eq!(strip_thinking("Use `cargo build` first."), "Use `cargo build` first.");
    }
}
