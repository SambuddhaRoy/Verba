//! Domain accuracy packs: vocabulary plus formatting rules, swappable per job.
//!
//! A flat custom-vocabulary list makes the user carry the whole burden of
//! knowing what the recogniser will get wrong. A pack is the same idea with the
//! work already done for a domain, and several can be on at once — someone
//! writing a clinical tool wants Medical and Code together.
//!
//! Three kinds of entry, because they are used differently downstream:
//!
//!   - `terms`      proper nouns and jargon. Corrected for casing after the
//!                  fact, *and* fed to the recogniser as bias so it has a
//!                  chance of hearing them right in the first place.
//!   - `hints`      `spoken => written` for things dictated one way and typed
//!                  another ("eye triple e" => "IEEE"). The written side is
//!                  worth biasing on; the spoken side is not.
//!   - `transforms` `spoken => written` for phrases that become symbols or
//!                  formatting ("open paren" => "("). Never biased: putting
//!                  "open paren" in a hotword list teaches the model to hear
//!                  those words *more* often, which is precisely backwards.
//!
//! All three collapse to the same (phrase, replacement) pairs that the existing
//! vocabulary mechanism in `pipeline::clean` already applies, so packs add data
//! rather than a second code path.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Pack {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub terms: Vec<String>,
    #[serde(default)]
    pub hints: Vec<String>,
    #[serde(default)]
    pub transforms: Vec<String>,
    /// False for the packs compiled in, true for anything loaded from disk.
    /// The UI uses it to decide whether deleting is offered.
    #[serde(default)]
    pub user: bool,
}

impl Pack {
    /// Every entry as `spoken => written`, ready for `vocabulary_pairs`.
    /// Transforms come last so a domain term always beats a generic symbol
    /// rule: the Code pack should not turn the word "dot" inside a spoken
    /// filename into a full stop before the term rule has had a look.
    pub fn entries(&self) -> Vec<String> {
        self.terms
            .iter()
            .chain(self.hints.iter())
            .chain(self.transforms.iter())
            .cloned()
            .collect()
    }

    /// What is worth telling the recogniser to listen for. Terms as written,
    /// and the written half of each hint. Transforms are deliberately absent.
    pub fn bias(&self) -> Vec<String> {
        let written = |e: &String| -> String {
            match e.split_once("=>") {
                Some((_, w)) => w.trim().to_string(),
                None => e.trim().to_string(),
            }
        };
        self.terms
            .iter()
            .map(written)
            .chain(self.hints.iter().map(written))
            // A symbol is not a word and biasing on it does nothing useful.
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .collect()
    }
}

/// Where user-authored packs live. One JSON file per pack.
pub fn dir() -> std::path::PathBuf {
    crate::config::dir().join("packs")
}

/// Built-in packs plus anything the user has dropped in `packs/`.
/// A user pack whose id collides with a built-in replaces it, so a shipped
/// pack can be corrected without waiting for a release.
pub fn all() -> Vec<Pack> {
    let mut packs: Vec<Pack> = builtin();

    if let Ok(entries) = std::fs::read_dir(dir()) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "json") {
                match std::fs::read_to_string(e.path())
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        // Same BOM tolerance as the config loader: an editor
                        // that adds one would otherwise make the pack vanish
                        // with no visible reason.
                        serde_json::from_str::<Pack>(s.trim_start_matches('\u{feff}'))
                            .map_err(|e| e.to_string())
                    }) {
                    Ok(mut p) => {
                        p.user = true;
                        if p.id.is_empty() {
                            crate::log!("pack {:?} has no id, ignored", e.path().display());
                            continue;
                        }
                        packs.retain(|existing| existing.id != p.id);
                        packs.push(p);
                    }
                    Err(err) => crate::log!("pack {} unreadable: {err}", e.path().display()),
                }
            }
        }
    }
    packs
}

/// The entries contributed by every enabled pack, in enable order.
pub fn active_entries(enabled: &[String]) -> Vec<String> {
    let packs = all();
    enabled
        .iter()
        .filter_map(|id| packs.iter().find(|p| &p.id == id))
        .flat_map(Pack::entries)
        .collect()
}

/// The bias terms contributed by every enabled pack.
pub fn active_bias(enabled: &[String]) -> Vec<String> {
    let packs = all();
    enabled
        .iter()
        .filter_map(|id| packs.iter().find(|p| &p.id == id))
        .flat_map(|p| p.bias())
        .collect()
}

fn pack(id: &str, name: &str, description: &str,
        terms: &[&str], hints: &[&str], transforms: &[&str]) -> Pack {
    let own = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect();
    Pack {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        terms: own(terms),
        hints: own(hints),
        transforms: own(transforms),
        user: false,
    }
}

/// The packs that ship with Verba.
pub fn builtin() -> Vec<Pack> {
    vec![
        pack(
            "code",
            "Code and programming",
            "Symbols, casing conventions and the names recognisers reliably mangle.",
            &[
                "GitHub", "GitLab", "Kubernetes", "PostgreSQL", "SQLite", "MySQL", "Redis",
                "nginx", "Docker", "TypeScript", "JavaScript", "Python", "Rust", "Golang",
                "async", "await", "enum", "struct", "impl", "stdout", "stderr", "stdin",
                "regex", "JSON", "YAML", "TOML", "HTTP", "HTTPS", "API", "CLI", "SDK",
                "OAuth", "JWT", "UUID", "CRUD", "ORM", "npm", "pnpm", "yarn", "cargo",
                "webpack", "Vite", "React", "Svelte", "Tauri", "WebAssembly",
            ],
            &[
                "dot com => .com",
                "dunder => __",
                "num pie => NumPy",
                "pie torch => PyTorch",
                "post gres => PostgreSQL",
                "sequel light => SQLite",
                "my sequel => MySQL",
                "java script => JavaScript",
                "type script => TypeScript",
                "node js => Node.js",
                "next js => Next.js",
                "vs code => VS Code",
                "see sharp => C#",
                "see plus plus => C++",
                "kay eight ess => k8s",
            ],
            &[
                "open paren => (",
                "close paren => )",
                "open bracket => [",
                "close bracket => ]",
                "open brace => {",
                "close brace => }",
                "open angle => <",
                "close angle => >",
                "backtick => `",
                "back slash => \\",
                "forward slash => /",
                "underscore => _",
                "double equals => ==",
                "triple equals => ===",
                "not equals => !=",
                "fat arrow => =>",
                "thin arrow => ->",
                "double colon => ::",
                "semi colon => ;",
                "pipe pipe => ||",
                "ampersand ampersand => &&",
                "hash bang => #!",
                "dollar sign => $",
                "at sign => @",
                "percent sign => %",
                "caret => ^",
                "tilde => ~",
            ],
        ),
        pack(
            "medical",
            "Medical",
            "Clinical vocabulary, common abbreviations and dosage shorthand.",
            &[
                "hypertension", "hypotension", "tachycardia", "bradycardia", "arrhythmia",
                "myocardial", "infarction", "ischaemia", "ischemia", "angina", "embolism",
                "thrombosis", "pneumonia", "asthma", "COPD", "dyspnoea", "dyspnea",
                "diabetes", "hyperglycaemia", "hypoglycaemia", "insulin", "metformin",
                "amoxicillin", "ibuprofen", "paracetamol", "acetaminophen", "warfarin",
                "atorvastatin", "omeprazole", "prednisolone", "salbutamol", "amlodipine",
                "anaemia", "anemia", "sepsis", "oedema", "edema", "erythema", "pruritus",
                "dysphagia", "syncope", "vertigo", "migraine", "neuropathy", "arthritis",
                "osteoporosis", "hypothyroidism", "hyperthyroidism", "gastritis",
                "auscultation", "palpation", "percussion", "prophylaxis", "aetiology",
                "etiology", "prognosis", "comorbidity", "contraindicated", "titrate",
            ],
            &[
                "bee pee => BP",
                "aitch are => HR",
                "are are => RR",
                "oh two sat => O2 sat",
                "ee see gee => ECG",
                "ee kay gee => EKG",
                "em are eye => MRI",
                "see tee scan => CT scan",
                "see bee see => CBC",
                "eff bee see => FBC",
                "you and ee => U&E",
                "el ef tee => LFT",
                "gee eye => GI",
                "you tee eye => UTI",
                "eye vee => IV",
                "eye em => IM",
                "pee oh => PO",
                "pee are en => PRN",
                "bee eye dee => BID",
                "tee eye dee => TID",
                "cue dee => QD",
                "cue eight aitch => q8h",
                "milligrams => mg",
                "micrograms => mcg",
                "millilitres => mL",
                "milliliters => mL",
                "history of => hx of",
            ],
            &[
                "slash => /",
                "over => /",
                "degrees celsius => °C",
                "plus minus => ±",
                "greater than or equal => ≥",
                "less than or equal => ≤",
            ],
        ),
        pack(
            "legal",
            "Legal",
            "Citation shorthand, Latin terms and drafting conventions.",
            &[
                "plaintiff", "defendant", "appellant", "respondent", "affidavit",
                "deposition", "subpoena", "injunction", "indemnity", "indemnification",
                "covenant", "warranty", "arbitration", "mediation", "jurisdiction",
                "statute", "tort", "negligence", "liability", "damages", "consideration",
                "estoppel", "novation", "assignment", "severability", "arbitrator",
                "solicitor", "barrister", "counsel", "paralegal", "litigation",
                "adjudication", "discovery", "disclosure", "pleadings", "interrogatories",
                "res judicata", "stare decisis", "prima facie", "bona fide", "pro rata",
                "ultra vires", "inter alia", "amicus curiae", "habeas corpus",
                "force majeure", "quantum meruit", "voir dire", "sub judice",
            ],
            &[
                "versus => v.",
                "et al => et al.",
                "ibid => ibid.",
                "see also => See also,",
                "section sign => §",
                "paragraph sign => ¶",
                "el el see => LLC",
                "el el pee => LLP",
                "pee el see => plc",
                "in cee => Inc.",
                "limited => Ltd.",
                "you ess see => U.S.C.",
                "see eff are => C.F.R.",
                "eff supp => F. Supp.",
                "notwithstanding the foregoing => Notwithstanding the foregoing,",
            ],
            &[
                "section symbol => §",
                "paragraph symbol => ¶",
                "open quote => \"",
                "close quote => \"",
                "em dash => —",
                "en dash => –",
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the whole module rests on: a symbol rule must never be
    /// handed to the recogniser as something to listen for. Biasing on "open
    /// paren" would make it hear those words more often, which is the opposite
    /// of what the rule is for.
    #[test]
    fn transforms_are_never_biased() {
        let code = builtin().into_iter().find(|p| p.id == "code").unwrap();
        let bias = code.bias();

        assert!(bias.iter().any(|t| t == "PostgreSQL"), "terms must be biased");
        assert!(bias.iter().any(|t| t == "NumPy"), "the written half of a hint must be biased");

        for b in &bias {
            assert!(!b.contains("paren"), "{b:?} came from a transform");
            assert!(b.chars().any(char::is_alphanumeric), "{b:?} is pure punctuation");
        }
        // The transform still has to exist as a rewrite rule.
        assert!(code.entries().iter().any(|e| e == "open paren => ("));
    }

    /// Every built-in entry has to survive the parser that pipeline::clean
    /// uses, or it is silently inert — present in the UI, doing nothing.
    #[test]
    fn every_builtin_entry_parses() {
        for p in builtin() {
            assert!(!p.id.is_empty() && !p.name.is_empty(), "{:?} is unnamed", p.id);
            for e in p.entries() {
                let (spoken, written) = match e.split_once("=>") {
                    Some((s, w)) => (s.trim(), w.trim()),
                    None => (e.trim(), e.trim()),
                };
                assert!(!spoken.is_empty(), "{:?}: empty spoken side in {e:?}", p.id);
                assert!(!written.is_empty(), "{:?}: empty written side in {e:?}", p.id);
                // Capitals on the spoken side are fine — vocabulary_pairs
                // lowercases the key before matching, which is what lets a
                // bare term like "GitHub" serve as both the pattern and the
                // replacement.
                assert!(
                    !spoken.contains("=>"),
                    "{:?}: {e:?} has more than one arrow, so the split is ambiguous", p.id
                );
            }
        }
    }

    /// Enabling several packs has to compose, since that is the stated point of
    /// packs over one flat list.
    #[test]
    fn multiple_packs_compose() {
        let both = active_entries(&["code".into(), "medical".into()]);
        assert!(both.iter().any(|e| e.starts_with("open paren")));
        assert!(both.iter().any(|e| e.starts_with("bee pee")));

        let none = active_entries(&[]);
        assert!(none.is_empty());

        // An id that does not exist must be skipped, not panic: a config can
        // easily name a user pack whose file has since been deleted.
        let ghost = active_entries(&["code".into(), "does-not-exist".into()]);
        assert!(!ghost.is_empty());
    }

    /// Ids have to be unique or `active_entries` picks an arbitrary one.
    #[test]
    fn builtin_ids_are_unique() {
        let mut ids: Vec<String> = builtin().into_iter().map(|p| p.id).collect();
        let before = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate pack id");
    }
}
