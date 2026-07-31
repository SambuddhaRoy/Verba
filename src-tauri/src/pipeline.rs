//! Turning a raw transcript into the text that actually gets inserted.
//!
//! Two stages, deliberately separate:
//!
//!   1. Deterministic rules — spoken punctuation, filler removal, vocabulary.
//!      Instant, predictable, and never invents a word. This is the whole of
//!      Raw mode, so Raw costs nothing beyond the transcription itself.
//!   2. An optional local model pass, per mode, over the output of stage one.
//!
//! Keeping them apart means the model always receives cleaned input, and that
//! a mode which must not paraphrase can simply switch stage two off rather
//! than being talked out of it by a prompt.

use crate::config::{Config, Mode};

/// Spoken forms that become punctuation. Longest first, so "question mark"
/// is tried before "mark" could match anything.
const SPOKEN: &[(&[&str], &str)] = &[
    (&["new", "paragraph"], "\n\n"),
    (&["new", "line"], "\n"),
    (&["question", "mark"], "?"),
    (&["exclamation", "mark"], "!"),
    (&["exclamation", "point"], "!"),
    (&["open", "paren"], "("),
    (&["close", "paren"], ")"),
    (&["open", "quote"], "\""),
    (&["close", "quote"], "\""),
    (&["full", "stop"], "."),
    (&["semicolon"], ";"),
    (&["period"], "."),
    (&["comma"], ","),
    (&["colon"], ":"),
];

/// Punctuation that attaches to the word before it with no space.
const HUGS_LEFT: &[&str] = &[".", ",", "?", "!", ":", ";", ")", "\""];

fn strip_edge_punctuation(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
}

/// Apply a vocabulary entry. A bare term corrects casing on a whole-word
/// match; `spoken => written` rewrites one phrase into another.
fn vocabulary_pairs(vocab: &[String]) -> Vec<(Vec<String>, String)> {
    vocab
        .iter()
        .filter_map(|entry| {
            let (spoken, written) = match entry.split_once("=>") {
                Some((s, w)) => (s.trim(), w.trim()),
                None => (entry.trim(), entry.trim()),
            };
            if spoken.is_empty() || written.is_empty() {
                return None;
            }
            let key = spoken.split_whitespace().map(|w| w.to_lowercase()).collect::<Vec<_>>();
            (!key.is_empty()).then(|| (key, written.to_string()))
        })
        .collect()
}

/// Stage one. Pure: same input, same output, no I/O.
pub fn clean(raw: &str, cfg: &Config, mode: &Mode) -> String {
    let words: Vec<&str> = raw.split_whitespace().collect();
    let vocab = vocabulary_pairs(&cfg.vocabulary);
    let fillers: Vec<String> = cfg.fillers.iter().map(|f| f.to_lowercase()).collect();

    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;

    'outer: while i < words.len() {
        // Vocabulary first: an entry may legitimately contain a word that
        // would otherwise be read as punctuation or filler.
        for (key, written) in &vocab {
            if i + key.len() <= words.len() {
                let hit = key.iter().enumerate().all(|(k, part)| {
                    strip_edge_punctuation(words[i + k]).eq_ignore_ascii_case(part)
                });
                if hit {
                    out.push(written.clone());
                    i += key.len();
                    continue 'outer;
                }
            }
        }

        if mode.spoken_punctuation {
            for (phrase, mark) in SPOKEN {
                if i + phrase.len() <= words.len() {
                    let hit = phrase.iter().enumerate().all(|(k, part)| {
                        strip_edge_punctuation(words[i + k]).eq_ignore_ascii_case(part)
                    });
                    if hit {
                        out.push((*mark).to_string());
                        i += phrase.len();
                        continue 'outer;
                    }
                }
            }
        }

        if mode.strip_fillers {
            let bare = strip_edge_punctuation(words[i]).to_lowercase();
            if fillers.iter().any(|f| *f == bare) {
                i += 1;
                continue;
            }
        }

        out.push(words[i].to_string());
        i += 1;
    }

    join(&out)
}

/// Re-assemble tokens, attaching punctuation and capitalising sentences.
fn join(tokens: &[String]) -> String {
    let mut s = String::new();
    let mut capitalise = true;

    for tok in tokens {
        if tok == "\n" || tok == "\n\n" {
            s.push_str(tok);
            capitalise = true;
            continue;
        }
        let hugs = HUGS_LEFT.contains(&tok.as_str());
        if !s.is_empty() && !hugs && !s.ends_with('\n') {
            s.push(' ');
        }
        if capitalise && !hugs {
            let mut c = tok.chars();
            if let Some(first) = c.next() {
                s.extend(first.to_uppercase());
                s.push_str(c.as_str());
            }
            capitalise = false;
        } else {
            s.push_str(tok);
        }
        // A sentence ends on these, so the next word starts a new one.
        if matches!(tok.as_str(), "." | "?" | "!") {
            capitalise = true;
        }
    }
    s.trim().to_string()
}

/// Full pipeline for a finished transcript.
///
/// A model failure is not fatal: the cleaned text is still good, and losing a
/// dictation because Ollama is not running would be a worse outcome than
/// inserting slightly less polished words.
pub fn run(raw: &str, cfg: &Config, exe: &str, title: &str) -> (String, String) {
    let mode = cfg.mode_for(exe, title);
    let cleaned = clean(raw, cfg, mode);

    if !mode.llm || cleaned.is_empty() {
        return (cleaned, mode.name.clone());
    }
    match crate::llm::rewrite(cfg, &mode.instructions, &cleaned) {
        Ok(text) if !text.trim().is_empty() => (text.trim().to_string(), mode.name.clone()),
        Ok(_) => {
            crate::log!("  {} returned nothing, inserting cleaned text", cfg.llm_model);
            (cleaned, mode.name.clone())
        }
        Err(e) => {
            crate::log!("  {} pass failed ({e}), inserting cleaned text", cfg.llm_model);
            (cleaned, format!("{} (raw)", mode.name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppRule, Config, Mode};

    fn mode(llm: bool) -> Mode {
        Mode { id: "t".into(), name: "T".into(), llm, ..Default::default() }
    }

    #[test]
    fn spoken_punctuation_attaches_without_a_space() {
        let cfg = Config::default();
        assert_eq!(
            clean("hello there comma how are you question mark", &cfg, &mode(false)),
            "Hello there, how are you?"
        );
    }

    #[test]
    fn sentences_capitalise_after_terminators() {
        let cfg = Config::default();
        assert_eq!(
            clean("that works period ship it period", &cfg, &mode(false)),
            "That works. Ship it."
        );
    }

    #[test]
    fn newlines_are_real_breaks() {
        let cfg = Config::default();
        assert_eq!(
            clean("first new line second", &cfg, &mode(false)),
            "First\nSecond"
        );
    }

    #[test]
    fn fillers_go_but_real_words_stay() {
        let cfg = Config::default();
        // "so" and "like" are not in the default list on purpose: they are
        // ordinary words far more often than they are filler.
        assert_eq!(
            clean("um so I uh like the plan", &cfg, &mode(false)),
            "So I like the plan"
        );
    }

    #[test]
    fn vocabulary_fixes_casing_and_rewrites_phrases() {
        let cfg = Config {
            vocabulary: vec!["ONNX".into(), "on ex => ONNX".into(), "Kubernetes".into()],
            ..Default::default()
        };
        assert_eq!(clean("we ship onnx today", &cfg, &mode(false)), "We ship ONNX today");
        assert_eq!(clean("we ship on ex today", &cfg, &mode(false)), "We ship ONNX today");
        assert_eq!(clean("run it on kubernetes", &cfg, &mode(false)), "Run it on Kubernetes");
    }

    #[test]
    fn code_mode_leaves_spoken_symbols_for_the_model() {
        let cfg = Config::default();
        let code = cfg.mode("code").unwrap().clone();
        // spoken_punctuation is off for code, so "period" survives as a word.
        assert!(clean("self period name", &cfg, &code).contains("period"));
    }

    #[test]
    fn app_rules_pick_the_mode_and_first_match_wins() {
        let cfg = Config::default();
        assert_eq!(cfg.mode_for("Code.exe", "main.rs").id, "code");
        assert_eq!(cfg.mode_for("OUTLOOK.EXE", "Re: pricing").id, "email");
        // Unknown app falls through to the default.
        assert_eq!(cfg.mode_for("notepad.exe", "Untitled").id, "raw");
        // Exe matching ignores case, since Windows reports it inconsistently.
        assert_eq!(cfg.mode_for("code.exe", "").id, "code");
    }

    #[test]
    fn title_condition_narrows_a_rule() {
        let mut cfg = Config::default();
        cfg.rules.insert(0, AppRule {
            mode: "email".into(),
            exe: vec!["chrome.exe".into()],
            title: Some("gmail".into()),
        });
        assert_eq!(cfg.mode_for("chrome.exe", "Inbox - Gmail").id, "email");
        assert_eq!(cfg.mode_for("chrome.exe", "Hacker News").id, "raw");
    }

    #[test]
    fn a_rule_naming_a_deleted_mode_still_dictates() {
        let mut cfg = Config::default();
        cfg.rules = vec![AppRule { mode: "gone".into(), exe: vec!["x.exe".into()], title: None }];
        cfg.default_mode = "also-gone".into();
        // Must not panic and must not return nothing to insert.
        assert!(!cfg.mode_for("x.exe", "").id.is_empty());
    }
}
