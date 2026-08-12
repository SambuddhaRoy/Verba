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
/// Punctuation that attaches to the word before it.
///
/// Opening brackets are here as well as in HUGS_RIGHT, so they close up on
/// both sides: `print(hello)` rather than `print ( hello)`. English prose
/// would want a space before an opening bracket, but these characters only
/// ever reach this function from a pack transform — dictating "open paren"
/// with no pack enabled leaves the words alone — and every pack that emits one
/// is code-oriented. The trade-off is a transcript where the recogniser itself
/// emitted a bracket mid-sentence, which is rare enough to accept.
const HUGS_LEFT: &[&str] =
    &[".", ",", "?", "!", ":", ";", ")", "]", "\"", "(", "["];

/// Punctuation that attaches to what *follows* it. Without this the Code pack
/// turns "print open paren hello close paren" into "Print ( hello)" — the
/// closing bracket hugs correctly and the opening one floats, which looks more
/// broken than leaving the words alone would have.
/// Braces and angles are deliberately absent from both lists: `if x == y {
/// return true }` is what a brace wants, whereas closing up like a paren gives
/// `y{return true}`. Angles are usually comparison operators when dictated, and
/// those want ordinary spacing too.
const HUGS_RIGHT: &[&str] = &["(", "[", "@", "#", "$", "~", "^", "_", "/", "\\", "§", "¶"];

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
    // The user's own entries first, then every enabled pack's.
    let vocab = vocabulary_pairs(&cfg.effective_vocabulary());
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

        // Recognisers emit the pronoun lowercase about as often as not, and no
        // English sentence wants it that way.
        if words[i] == "i" {
            out.push("I".into());
            i += 1;
            continue;
        }
        if let Some(rest) = words[i].strip_prefix("i'") {
            out.push(format!("I'{rest}"));
            i += 1;
            continue;
        }

        out.push(words[i].to_string());
        i += 1;
    }

    join(&out)
}

/// Numbers carry the most meaning per token in dictation — prices, dates,
/// counts, versions — and are the least recoverable if a rewrite drops them.
fn numbers(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '.')
        .filter(|t| t.chars().any(|c| c.is_ascii_digit()))
        .map(|t| t.trim_matches('.').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Would accepting this rewrite lose the user's words?
///
/// A model that truncates, or silently drops a figure, is worse than no model
/// at all: the user cannot see what went missing because the original is gone
/// by the time the text lands. Measured, a 40-word dictation came back from
/// Notes mode as a single seven-word bullet — this is the guard for that.
fn survives(original: &str, rewritten: &str) -> Result<(), String> {
    let before = original.split_whitespace().count();
    let after = rewritten.split_whitespace().count();
    // Bullets and tightened prose legitimately shed filler, so the floor is
    // generous; it is only catching collapse, not compression.
    if before >= 12 && after * 100 < before * 40 {
        return Err(format!("dropped {before} words to {after}"));
    }
    for n in numbers(original) {
        if !rewritten.contains(&n) {
            return Err(format!("lost the figure {n:?}"));
        }
    }
    Ok(())
}

/// Re-assemble tokens, attaching punctuation and capitalising sentences.
fn join(tokens: &[String]) -> String {
    let mut s = String::new();
    let mut capitalise = true;
    /// Set by an opening bracket so the next token joins it directly.
    let mut glue_next = false;

    for tok in tokens {
        if tok == "\n" || tok == "\n\n" {
            s.push_str(tok);
            capitalise = true;
            glue_next = false;
            continue;
        }
        let hugs = HUGS_LEFT.contains(&tok.as_str());
        if !s.is_empty() && !hugs && !glue_next && !s.ends_with('\n') {
            s.push(' ');
        }
        glue_next = HUGS_RIGHT.contains(&tok.as_str());
        // A symbol cannot carry a capital, so it must not use up the pending
        // one either: "(hello" at the start of a sentence should still yield
        // "(Hello", not leave the word lowercase behind the bracket.
        let capitalisable = tok.chars().any(char::is_alphabetic);
        if capitalise && !hugs && capitalisable {
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
        Ok(text) if !text.trim().is_empty() => {
            let text = text.trim();
            match survives(&cleaned, text) {
                Ok(()) => (text.to_string(), mode.name.clone()),
                Err(why) => {
                    crate::log!("  rewrite rejected ({why}), inserting cleaned text");
                    (cleaned, format!("{} (unchanged)", mode.name))
                }
            }
        }
        Ok(_) => {
            crate::log!("  {} returned nothing, inserting cleaned text", cfg.llm_model);
            (cleaned, format!("{} (unchanged)", mode.name))
        }
        Err(e) => {
            crate::log!("  {} pass failed ({e}), inserting cleaned text", cfg.llm_model);
            (cleaned, format!("{} (unchanged)", mode.name))
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

    /// Pack transforms produce bare symbols, and the spacing rules have to
    /// handle both directions or the output looks worse than the raw words.
    #[test]
    fn brackets_attach_to_what_they_enclose() {
        let cfg = Config {
            enabled_packs: vec!["code".into()],
            ..Default::default()
        };
        crate::config::invalidate_vocabulary();
        let out = clean("print open paren hello close paren", &cfg, &mode(false));
        crate::config::invalidate_vocabulary();
        assert_eq!(out, "Print(hello)");
    }

    #[test]
    fn code_mode_leaves_spoken_symbols_for_the_model() {
        let cfg = Config::default();
        let code = cfg.mode("code").unwrap().clone();
        // spoken_punctuation is off for code, so "period" survives as a word.
        assert!(clean("self period name", &cfg, &code).contains("period"));
    }

    #[test]
    fn the_pronoun_is_capitalised() {
        let cfg = Config::default();
        assert_eq!(
            clean("i think i'll ship it", &cfg, &mode(false)),
            "I think I'll ship it"
        );
        // Only the standalone pronoun — not any word starting with i.
        assert_eq!(clean("in it", &cfg, &mode(false)), "In it");
    }

    #[test]
    fn a_collapsed_rewrite_is_rejected() {
        let long = "thanks for sending the deck over i went through the pricing \
                    section this morning and it mostly holds up one thing i would \
                    change is the enterprise tier";
        // What Notes mode actually returned before the instructions were fixed.
        assert!(super::survives(long, "* Thanks for sending the deck over.").is_err());
        // Genuine tightening is not collapse and must pass.
        assert!(super::survives(long, long).is_ok());
    }

    #[test]
    fn a_rewrite_that_drops_a_figure_is_rejected() {
        let src = "push the seat minimum to 25 and the price to 4.50";
        assert!(super::survives(src, "Raise the seat minimum to 25 and the price to 4.50").is_ok());
        assert!(super::survives(src, "Raise the seat minimum and the price to 4.50").is_err());
    }

    #[test]
    fn short_utterances_are_exempt_from_the_length_floor() {
        // "yes" -> "Yes." is a legitimate rewrite, not a collapse.
        assert!(super::survives("yeah ok sure", "Yes.").is_ok());
    }

    #[test]
    fn terminals_get_raw_so_commands_are_never_rewritten() {
        let cfg = Config::default();
        for shell in ["WindowsTerminal.exe", "pwsh.exe", "cmd.exe"] {
            let m = cfg.mode_for(shell, "");
            assert_eq!(m.id, "raw", "{shell} must not be rewritten");
            assert!(!m.llm);
        }
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
