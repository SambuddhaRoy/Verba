//! Adaptive personalisation: learn what this user keeps having to fix.
//!
//! When a dictation is corrected, the difference between what the recogniser
//! produced and what the user actually wanted is the most direct evidence there
//! is about the gap between the model and this person's vocabulary. A name, a
//! product, a piece of jargon — the same word gets fixed over and over.
//!
//! What we do with that is deliberately conservative: a term corrected enough
//! times is fed to the recogniser as *bias* — whisper's `initial_prompt`,
//! sherpa's hotwords — so it has a better chance of hearing it right next time.
//! It is never turned into an automatic find-and-replace. A substitution that
//! was right once is not right always: someone who corrected "there" to "their"
//! in one sentence would have every later "there" silently corrupted, and a
//! dictation tool that quietly changes words you did say is worse than one that
//! occasionally mishears.
//!
//! Off until switched on. The store is line-delimited JSON in plain text rather
//! than a database — it is a few hundred rows of the user's own words, and a
//! file they can read, grep and delete themselves is a better answer for
//! something this sensitive than an opaque one. If it ever needs full-text
//! search over the whole dictation history, that is the point to reach for
//! SQLite; for counting corrections it would be ceremony.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// How many times a term must be corrected before it is worth biasing on.
/// One correction is as likely to be a typo, a change of mind, or a one-off
/// proper noun as it is a systematic mishearing.
const PROMOTE_AT: u32 = 2;

/// Before a correction is applied *deterministically* rather than merely
/// suggested to the recogniser. Higher than PROMOTE_AT because the failure
/// modes are not comparable: a bad bias term slightly skews a probability, a
/// bad rewrite silently changes words the user actually said.
const REWRITE_AT: u32 = 3;

/// Words common enough that rewriting them automatically would eventually
/// corrupt something the user meant. Deliberately the function words and
/// homophone traps — "there/their", "to/too", "its/it's" — since those are
/// exactly what a one-off context-specific correction looks like.
///
/// Not a dictionary, and not trying to be: it only has to be good enough that
/// an automatic rewrite never fires on an ordinary English word. A proper noun
/// or a piece of jargon, which is what this feature is for, is never in here.
const COMMON: &[&str] = &[
    "a", "about", "after", "all", "also", "an", "and", "any", "are", "as", "at", "back", "be",
    "because", "been", "before", "being", "but", "by", "can", "come", "could", "day", "do",
    "down", "each", "even", "first", "for", "from", "get", "give", "go", "good", "great", "had",
    "has", "have", "he", "her", "here", "hers", "him", "his", "how", "i", "if", "in", "into",
    "is", "it", "its", "it's", "just", "know", "like", "look", "make", "man", "many", "me",
    "more", "most", "my", "new", "no", "not", "now", "of", "on", "one", "only", "or", "other",
    "our", "out", "over", "own", "people", "run", "said", "same", "say", "see", "she", "so",
    "some", "take", "than", "that", "the", "their", "theirs", "them", "then", "there",
    "there's", "these", "they", "they're", "thing", "think", "this", "those", "through",
    "time", "to", "too", "two", "up", "us", "use", "very", "want", "was", "way", "we", "well",
    "were", "we're", "what", "when", "where", "which", "while", "who", "why", "will", "with",
    "would", "year", "you", "your", "you're", "yours",
];

/// Whether a learned substitution is safe to apply without the model's help.
///
/// The test is on the *wrong* side: if what the recogniser produced is itself
/// ordinary English, the correction was probably about this one sentence, and
/// applying it everywhere would change words the user did say. "cuber netties"
/// is safe to rewrite; "there" is not, however many times it was fixed.
fn safe_to_rewrite(wrong: &str) -> bool {
    let words: Vec<&str> = wrong.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    !words
        .iter()
        .all(|w| COMMON.contains(&w.to_lowercase().trim_matches('\'')))
}

/// Whisper's prompt is capped at half the text context — about 224 tokens — and
/// anything past that is silently dropped, taking the terms that mattered with
/// it. Bias lists are kept well inside that, most-corrected first.
pub const MAX_BIAS_TERMS: usize = 64;

/// A single edit: what came out, and what it should have said.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Correction {
    /// Unix seconds. Absolute so the file stays meaningful on its own.
    pub at: u64,
    pub raw: String,
    pub fixed: String,
}

/// One learned substitution, aggregated across every correction seen.
#[derive(Serialize, Clone, Debug)]
pub struct Learned {
    pub wrong: String,
    pub right: String,
    pub count: u32,
    /// True once it has been corrected often enough to be biased on.
    pub promoted: bool,
    /// True once it is also applied deterministically. Needed because the
    /// engine the user is on may have no biasing at all — sherpa's hotwords
    /// cannot encode arbitrary words against Parakeet's BPE vocabulary, so on
    /// that engine a rewrite is the only way a learned correction can take
    /// effect.
    pub rewrite: bool,
}

pub fn path() -> std::path::PathBuf {
    crate::config::dir().join("corrections.jsonl")
}

/// Aggregation is over the whole file, so it is cached and rebuilt only when
/// something is appended or the file is cleared.
static CACHE: Mutex<Option<Vec<Learned>>> = Mutex::new(None);

fn invalidate() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Record one correction. Returns the substitutions it contributed.
pub fn record(raw: &str, fixed: &str) -> Result<Vec<(String, String)>, String> {
    let pairs = substitutions(raw, fixed);
    if raw.trim() == fixed.trim() {
        return Ok(pairs);
    }

    let entry = Correction {
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        raw: raw.to_string(),
        fixed: fixed.to_string(),
    };

    let dir = crate::config::dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    use std::io::Write;
    let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path())
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())?;

    invalidate();
    Ok(pairs)
}

/// Every correction on file, oldest first. A malformed line is skipped rather
/// than failing the read: one bad row must not cost the user the whole history.
pub fn history() -> Vec<Correction> {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Correction>(l).ok())
        .collect()
}

/// Learned substitutions, most-corrected first.
pub fn learned() -> Vec<Learned> {
    if let Some(cached) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return cached;
    }

    let mut counts: std::collections::HashMap<(String, String), u32> = Default::default();
    for c in history() {
        for pair in substitutions(&c.raw, &c.fixed) {
            *counts.entry(pair).or_insert(0) += 1;
        }
    }

    let mut out: Vec<Learned> = counts
        .into_iter()
        .map(|((wrong, right), count)| Learned {
            promoted: count >= PROMOTE_AT,
            rewrite: count >= REWRITE_AT && safe_to_rewrite(&wrong),
            wrong,
            right,
            count,
        })
        .collect();
    // Most corrected first, then alphabetically so the order is stable in the
    // UI rather than shuffling on every rebuild.
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.right.cmp(&b.right)));

    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(out.clone());
    out
}

/// The terms worth telling the recogniser to expect.
pub fn bias_terms() -> Vec<String> {
    learned()
        .into_iter()
        .filter(|l| l.promoted)
        .map(|l| l.right)
        .collect()
}

/// Learned corrections safe to apply as rewrites, in the `spoken => written`
/// grammar the vocabulary mechanism already understands.
pub fn rewrite_entries() -> Vec<String> {
    learned()
        .into_iter()
        .filter(|l| l.rewrite)
        .map(|l| format!("{} => {}", l.wrong, l.right))
        .collect()
}

/// Delete the history. The user's own words, so removing them has to be one
/// action and has to actually remove them.
pub fn clear() -> Result<(), String> {
    match std::fs::remove_file(path()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    invalidate();
    Ok(())
}

// --- diffing --------------------------------------------------------------

/// Longest common subsequence over words, as a table of lengths.
fn lcs(a: &[&str], b: &[&str]) -> Vec<Vec<usize>> {
    let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if same(a[i], b[j]) {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    t
}

/// Words match if they differ only by surrounding punctuation. Case is
/// significant: "github" becoming "GitHub" is exactly the kind of correction
/// this is for, and folding case would throw it away.
fn same(a: &str, b: &str) -> bool {
    trim(a) == trim(b)
}

fn trim(w: &str) -> &str {
    w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-' && c != '_')
}

/// The substitutions that turn `raw` into `fixed`.
///
/// Only balanced replacements of a few words are kept. A long run, or text
/// purely added or deleted, is the user rewriting a sentence rather than fixing
/// a misheard word, and learning from it would poison the bias list with
/// whatever they happened to be writing about.
pub fn substitutions(raw: &str, fixed: &str) -> Vec<(String, String)> {
    /// Beyond this, it is a rewrite, not a correction.
    const MAX_RUN: usize = 3;

    let a: Vec<&str> = raw.split_whitespace().collect();
    let b: Vec<&str> = fixed.split_whitespace().collect();
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }

    let t = lcs(&a, &b);
    let (mut i, mut j) = (0usize, 0usize);
    let mut out = Vec::new();

    // Continue while *either* side has words left. Stopping as soon as one is
    // exhausted drops a replacement at the very end of the text — the run gets
    // collected on one side and never on the other, so it looks like a pure
    // deletion and is discarded.
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && same(a[i], b[j]) {
            i += 1;
            j += 1;
            continue;
        }

        // Collect the whole divergent run on both sides at once.
        let (si, sj) = (i, j);
        while (i < a.len() || j < b.len())
            && !(i < a.len() && j < b.len() && same(a[i], b[j]))
        {
            if j >= b.len() {
                i += 1;
            } else if i >= a.len() {
                j += 1;
            } else if t[i + 1][j] >= t[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        let del = &a[si..i];
        let ins = &b[sj..j];

        if !del.is_empty()
            && !ins.is_empty()
            && del.len() <= MAX_RUN
            && ins.len() <= MAX_RUN
        {
            let wrong = del.iter().map(|w| trim(w)).collect::<Vec<_>>().join(" ");
            let right = ins.iter().map(|w| trim(w)).collect::<Vec<_>>().join(" ");
            // A replacement has to actually name something. Pure punctuation
            // or an empty side teaches the recogniser nothing.
            if !wrong.is_empty()
                && !right.is_empty()
                && wrong != right
                && right.chars().any(char::is_alphanumeric)
            {
                out.push((wrong, right));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subs(a: &str, b: &str) -> Vec<(String, String)> {
        substitutions(a, b)
    }

    /// The case the whole feature exists for: one word misheard, fixed, and
    /// recognised as a substitution rather than as a rewrite.
    #[test]
    fn a_single_misheard_word_is_learned() {
        assert_eq!(
            subs("we deployed to cuber netties yesterday", "we deployed to Kubernetes yesterday"),
            vec![("cuber netties".to_string(), "Kubernetes".to_string())]
        );
        assert_eq!(
            subs("push it to git hub", "push it to GitHub"),
            vec![("git hub".to_string(), "GitHub".to_string())]
        );
    }

    /// Casing alone is a real correction — arguably the most common one for
    /// product names — so folding case would discard the main signal.
    #[test]
    fn casing_only_changes_still_count() {
        assert_eq!(
            subs("i use postgresql daily", "i use PostgreSQL daily"),
            vec![("postgresql".to_string(), "PostgreSQL".to_string())]
        );
    }

    /// Punctuation drifting around a word is not a correction of the word.
    #[test]
    fn punctuation_alone_is_not_a_correction() {
        assert!(subs("hello there", "hello, there.").is_empty());
        assert!(subs("done", "done!").is_empty());
    }

    /// Rewriting a sentence must not be mined for vocabulary: the terms would
    /// be whatever the user was writing about, not something misheard.
    #[test]
    fn wholesale_rewrites_are_ignored() {
        let long = subs(
            "the quick brown fox jumps over the lazy dog",
            "a completely different sentence about something else entirely",
        );
        assert!(long.is_empty(), "got {long:?}");
    }

    /// Pure insertions and deletions carry no wrong-to-right mapping.
    #[test]
    fn additions_and_removals_are_not_substitutions() {
        assert!(subs("send the report", "send the report today").is_empty());
        assert!(subs("send the report today", "send the report").is_empty());
    }

    /// Two independent fixes in one dictation are both worth having.
    #[test]
    fn several_corrections_in_one_pass() {
        let got = subs(
            "we use post gres and react native",
            "we use PostgreSQL and React Native",
        );
        assert!(got.iter().any(|(_, r)| r == "PostgreSQL"), "got {got:?}");
        assert!(got.iter().any(|(_, r)| r.contains("React")), "got {got:?}");
    }

    /// Identical text must produce nothing, or opening the fix window and
    /// closing it again would teach the model noise.
    #[test]
    fn no_change_learns_nothing() {
        assert!(subs("nothing changed here", "nothing changed here").is_empty());
        assert!(subs("", "").is_empty());
        assert!(subs("something", "").is_empty());
    }

    /// A correction at the very end is the case a naive diff walk drops,
    /// because the loop stops when either side runs out.
    #[test]
    fn a_correction_at_the_end_is_caught() {
        let got = subs("deploy to heroko", "deploy to Heroku");
        assert_eq!(got, vec![("heroko".to_string(), "Heroku".to_string())]);
    }

    /// Promotion is what stops a single stray edit from steering the model.
    #[test]
    fn promotion_needs_repetition() {
        assert!(1 < PROMOTE_AT, "one correction must not steer the recogniser");
        assert!(PROMOTE_AT <= 2, "waiting too long makes the feature feel dead");
        assert!(REWRITE_AT > PROMOTE_AT,
                "rewriting is riskier than biasing and must need more evidence");
    }

    /// The guard on automatic rewriting. Getting this wrong silently changes
    /// words the user actually said, which is worse than any mishearing.
    #[test]
    fn only_unusual_wordings_are_rewritten_automatically() {
        // What the feature is for: jargon the recogniser mangles.
        assert!(safe_to_rewrite("cuber netties"));
        assert!(safe_to_rewrite("post gres"));
        assert!(safe_to_rewrite("heroko"));

        // Homophone fixes are context-specific. Applying one globally would
        // corrupt every later sentence that legitimately used the word.
        assert!(!safe_to_rewrite("there"));
        assert!(!safe_to_rewrite("their"));
        assert!(!safe_to_rewrite("to"));
        assert!(!safe_to_rewrite("its"));
        assert!(!safe_to_rewrite("it's"));
        assert!(!safe_to_rewrite("THERE"), "the check must ignore case");
        assert!(!safe_to_rewrite("in to"), "every word common means unsafe");
        assert!(!safe_to_rewrite(""));

        // A phrase mixing a common word with jargon is still worth rewriting:
        // the jargon is what makes it distinctive.
        assert!(safe_to_rewrite("the cuber netties"));
    }
}
