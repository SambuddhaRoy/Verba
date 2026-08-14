//! Every outbound request Verba makes, and a record of each one.
//!
//! The product's central claim is that dictation is local. Saying so is cheap;
//! this is the part that lets someone check. Every HTTP request goes through
//! `get` or `post` here, each is logged with its destination and the moment it
//! happened, and the settings window shows the log live. A fully local session
//! — dictate, format, insert — produces an empty list.
//!
//! ## What makes the list trustworthy
//!
//! Not discipline. A test walks the source and fails if `ureq::` appears
//! outside this file, so a request that skipped the log would break the build
//! rather than quietly not appear.
//!
//! ## What it cannot see
//!
//! Child processes. The faster-whisper and Parakeet sidecars are Python, and
//! when they install dependencies or fetch their own weights those connections
//! belong to `pip` and `huggingface_hub`, not to this process. The panel says
//! so rather than implying a completeness it does not have. Everything Verba
//! itself does — model downloads, Ollama, update checks — is here.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Enough to cover any plausible session without growing without bound. The
/// log is a diagnostic, not an archive, and it is deliberately memory-only:
/// writing a history of where the user's machine connected would be its own
/// small privacy problem.
const KEEP: usize = 200;

#[derive(Clone, serde::Serialize)]
pub struct Entry {
    /// Unix seconds.
    pub at: u64,
    pub method: &'static str,
    /// Host alone, for the common case of scanning the list at a glance.
    pub host: String,
    pub url: String,
    /// Why Verba made this request, in words a user can judge.
    pub purpose: &'static str,
    /// True when the destination is loopback, so a local Ollama call is not
    /// mistaken for the app phoning home.
    pub local: bool,
}

static LOG: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

/// Loopback, including the forms people actually write.
///
/// Bracket-aware because an IPv6 literal is full of colons: splitting
/// `[::1]:11434` on the first one leaves `[`, which matches nothing and would
/// have shown a local Ollama call as an outbound connection.
fn is_local(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None if host.matches(':').count() > 1 => host, // bare IPv6, no port
        None => host.split(':').next().unwrap_or(host),
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1") || name.starts_with("127.")
}

fn record(method: &'static str, url: &str, purpose: &'static str) {
    let host = host_of(url);
    let entry = Entry {
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        method,
        local: is_local(&host),
        host,
        url: url.to_string(),
        purpose,
    };

    // Logged as well as recorded: the panel is for users, the log file is what
    // gets attached to a bug report.
    crate::log!("net {} {} ({})", entry.method, entry.url, entry.purpose);

    let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if log.len() == KEEP {
        log.pop_front();
    }
    log.push_back(entry);
}

/// Newest first.
pub fn entries() -> Vec<Entry> {
    LOG.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .rev()
        .cloned()
        .collect()
}

pub fn clear() {
    LOG.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// A GET, recorded. `purpose` is shown to the user, so write it for them.
pub fn get(url: &str, purpose: &'static str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    record("GET", url, purpose);
    ureq::get(url)
}

/// A POST, recorded.
pub fn post(url: &str, purpose: &'static str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
    record("POST", url, purpose);
    ureq::post(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guarantee the audit panel rests on. If a request can be made
    /// without going through this module, the panel is decorative — so this
    /// fails the build rather than letting the claim quietly become false.
    #[test]
    fn no_request_bypasses_the_log() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();

        for entry in std::fs::read_dir(&dir).expect("src/ is readable") {
            let path = entry.expect("readable entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            // This file is where the wrapper lives, so it is the one place the
            // raw crate is allowed.
            if path.file_name().is_some_and(|n| n == "net.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable source");
            for (n, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or(line);
                if code.contains("ureq::") {
                    offenders.push(format!(
                        "{}:{}  {}",
                        path.file_name().unwrap().to_string_lossy(),
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these call ureq directly and would not appear in the network panel:\n  {}",
            offenders.join("\n  ")
        );
    }

    #[test]
    fn hosts_and_loopback_are_recognised() {
        assert_eq!(host_of("https://api.github.com/repos/x/y"), "api.github.com");
        assert_eq!(host_of("http://127.0.0.1:11434/api/tags"), "127.0.0.1:11434");

        assert!(is_local("127.0.0.1:11434"));
        assert!(is_local("localhost:11434"));
        assert!(is_local("[::1]:11434"));
        // The distinction the panel is for: these must never read as local.
        assert!(!is_local("api.github.com"));
        assert!(!is_local("huggingface.co"));
        // A host merely starting with the digits is not loopback.
        assert!(!is_local("127001.example.com"));
    }

    #[test]
    fn the_log_is_bounded_and_ordered() {
        clear();
        for i in 0..(KEEP + 20) {
            record("GET", &format!("https://example.test/{i}"), "test");
        }
        let all = entries();
        assert_eq!(all.len(), KEEP, "the log must not grow without bound");
        // Newest first, and the oldest have been dropped.
        assert!(all[0].url.ends_with(&format!("/{}", KEEP + 19)));
        clear();
        assert!(entries().is_empty());
    }
}
