//! Live typing: words reach the focused app while the user is still speaking.
//!
//! Interim transcripts change under you. A word that reads "pric" on one pass
//! reads "pricing" on the next, and "their" can become "there" once more audio
//! arrives. Typing each pass as it lands would leave the document full of
//! corrections, so a word is only typed once two passes in a row agree on it.
//! The final pass, which sees the whole recording, then fixes whatever still
//! differs.

use crate::config::{Config, Mode};
use crate::{focus, inject, pipeline};

/// Case and edge punctuation do not make two passes disagree.
fn norm(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_lowercase()
}

/// Which leading words of a changing transcript are settled.
#[derive(Default)]
pub struct Stable {
    prev: Vec<String>,
    committed: Vec<String>,
}

impl Stable {
    /// Feed the newest interim transcript. Returns the words safe to type,
    /// which only ever grows unless a settled word is revised.
    pub fn update(&mut self, text: &str) -> &[String] {
        let words: Vec<String> = text.split_whitespace().map(str::to_string).collect();
        let agreed = self
            .prev
            .iter()
            .zip(&words)
            .take_while(|(a, b)| norm(a) == norm(b))
            .count();
        if agreed > self.committed.len() {
            self.committed = words[..agreed].to_vec();
        }
        self.prev = words;
        &self.committed
    }
}

/// One dictation's worth of live typing.
pub struct Live {
    typist: inject::Typist,
    words: Stable,
    /// The window that had focus when the key went down. Text goes there or
    /// nowhere: if the user switches window mid-sentence, typing into the new
    /// one would put their dictation into the wrong document.
    hwnd: isize,
    stopped: bool,
}

impl Live {
    pub fn new(hwnd: isize) -> Self {
        Self { typist: inject::Typist::default(), words: Stable::default(), hwnd, stopped: false }
    }

    pub fn on_partial(&mut self, text: &str, cfg: &Config, mode: &Mode) {
        let settled = self.words.update(text);
        if settled.is_empty() {
            return;
        }
        let target = pipeline::clean(&settled.join(" "), cfg, mode);
        self.apply(&target);
    }

    /// The whole-recording result: type what is missing, correct what differs.
    pub fn finish(&mut self, text: &str, cfg: &Config, mode: &Mode) {
        let target = pipeline::clean(text, cfg, mode);
        self.apply(&target);
    }

    fn apply(&mut self, target: &str) {
        if self.stopped {
            return;
        }
        if focus::foreground_window() != self.hwnd {
            self.stopped = true;
            let now = focus::foreground();
            crate::log!("  live typing stopped: focus moved to [{}] {}", now.exe, now.title);
            return;
        }
        if let Err(e) = self.typist.set(target) {
            self.stopped = true;
            crate::log!("  live typing failed: {e}");
        }
    }

    /// What is in the document now, for "fix the last transcription".
    pub fn typed(&self) -> &str {
        self.typist.typed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(passes: &[&str]) -> Vec<String> {
        let mut s = Stable::default();
        let mut last = Vec::new();
        for p in passes {
            last = s.update(p).to_vec();
        }
        last
    }

    #[test]
    fn nothing_is_typed_from_a_single_pass() {
        assert!(feed(&["hello there"]).is_empty());
    }

    #[test]
    fn words_two_passes_agree_on_are_typed() {
        assert_eq!(feed(&["hello there", "hello there my friend"]), vec!["hello", "there"]);
    }

    #[test]
    fn a_word_still_changing_is_held_back() {
        // "pric" then "pricing": the last word has not settled.
        let out = feed(&["the pric", "the pricing"]);
        assert_eq!(out, vec!["the"]);
    }

    #[test]
    fn case_and_punctuation_do_not_break_agreement() {
        assert_eq!(feed(&["Hello, there", "hello there friend"]), vec!["hello", "there"]);
    }

    #[test]
    fn settled_words_never_shrink_when_a_later_pass_wobbles() {
        let mut s = Stable::default();
        s.update("one two three");
        assert_eq!(s.update("one two three four").len(), 3);
        // A pass that disagrees from the second word on must not retract.
        assert_eq!(s.update("one too three four five").len(), 3);
    }

    #[test]
    fn a_settled_word_can_be_revised_once_the_passes_agree_on_the_new_one() {
        let mut s = Stable::default();
        s.update("their cat sat");
        s.update("their cat sat down");
        s.update("there cat sat down now");
        let out = s.update("there cat sat down now please").to_vec();
        assert_eq!(out[0], "there");
        assert!(out.len() >= 4);
    }
}
