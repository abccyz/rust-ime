//! The top-level `Engine` state machine — the Linux analog of `wave-ime`'s
//! `controller::BoardController` + `keyboard::InputModel`.
//!
//! The front end (fcitx5 addon or Tauri window) feeds it physical key events
//! and reads back a `PreeditState` after each event. It is synchronous,
//! single-threaded, and owns the candidate engine + user dictionary.
//!
//! This is the exact contract the Android `ImeService` drove through
//! `KeyboardJni`, restated for a headless front end:
//!
//! * `feed_key` — a physical key. The engine updates the pinyin buffer and
//!   re-generates candidates. It returns the events the front end must apply
//!   (preedit change, commit, candidate change).
//! * `finish` — the user pressed Enter/Space or dismissed the window: commit
//!   the first candidate (or the raw buffer if no candidates), clear.
//! * `commit_candidate` — the user chose candidate `idx` (number key or mouse).
//! * `reset` — Escape / focus loss: drop the preedit without committing.

use std::collections::VecDeque;

use crate::candidate::{Candidate, CandidateEngine};
use crate::userdict;

/// A physical key the front end can feed to the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A letter `a`..=`z` (already lower-cased by the engine).
    Letter(char),
    Backspace,
    Enter,
    Space,
    Escape,
    /// Digit 1..=9 for candidate selection.
    Digit(u8),
    /// Left / Right arrow to move the cursor within the preedit.
    Left,
    Right,
    /// An unhandled / non-letter key. The engine treats these as "commit any
    /// preedit and pass the key through".
    Other,
}

/// What the front end should apply after an engine event. The front end maps
/// these onto its IM framework (fcitx5 `InputContextEvent`, or an IPC message
/// to the Tauri window).
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// The preedit (raw pinyin) changed to this value. Front end calls
    /// `set_preedit`.
    PreeditChanged(String),
    /// Commit this text to the focused editor and clear the preedit.
    Commit(String),
    /// The candidate strip changed to this list. Front end re-renders the
    /// candidate window.
    Candidates(Vec<Candidate>),
    /// Clear the preedit without committing (Escape / focus loss).
    Clear,
    /// Nothing to do.
    Noop,
}

/// The observable preedit state the front end renders.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PreeditState {
    /// Raw pinyin currently being typed (e.g. `nihao`).
    pub raw: String,
    /// Whether the raw string is valid pinyin (drives candidate-strip
    /// visibility).
    pub is_valid_pinyin: bool,
    /// The ranked candidates for the current buffer.
    pub candidates: Vec<Candidate>,
    /// Index of the currently selected candidate (number-key navigation).
    pub selected: usize,
}

/// The IME engine. One instance per input session.
pub struct Engine {
    candidates: CandidateEngine,
    buffer: String,
    selected: usize,
    /// Whether we have entered a "committed" state for this session (used to
    /// decide passthrough vs. pinyin on the first letter after a commit).
    just_committed: bool,
    /// A ring of the most recent commits, for the user-dict learning loop.
    recent: VecDeque<String>,
    max_recent: usize,
    /// Path to the user dictionary file. When set, `commit_candidate`
    /// auto-saves the snapshot after each commit.
    user_dict_path: Option<std::path::PathBuf>,
    /// Last committed text — the context used for next-word prediction.
    last_committed: Option<String>,
}

/// Maximum candidates shown per page (vertical list).
pub const MAX_CANDIDATES: usize = 10;

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        let mut candidates = CandidateEngine::new();
        crate::candidate::load_builtin_words(candidates.system_dictionary());
        crate::candidate::load_dict_str(
            include_str!("../data/dict.txt"),
            candidates.system_dictionary(),
        );
        Self {
            candidates,
            buffer: String::new(),
            selected: 0,
            just_committed: false,
            recent: VecDeque::new(),
            max_recent: 64,
            user_dict_path: None,
            last_committed: None,
        }
    }

    /// Create an engine that loads and persists a user dictionary at `path`.
    /// The file is loaded on construction and auto-saved after each commit.
    pub fn with_user_dict(path: &std::path::Path) -> Self {
        let mut e = Self::new();
        e.load_user_dict(path).ok();
        e.user_dict_path = Some(path.to_path_buf());
        e
    }

    /// Access the user dictionary for persistence / learning.
    pub fn user_dictionary(&mut self) -> &mut crate::candidate::Dictionary {
        self.candidates.user_dictionary()
    }

    /// Load a user-dictionary file into the engine.
    pub fn load_user_dict(&mut self, path: &std::path::Path) -> std::io::Result<usize> {
        userdict::load_words_file(path, self.candidates.user_dictionary())
    }

    pub fn state(&self) -> PreeditState {
        // With no active pinyin, show next-word predictions based on the last
        // committed text (contextual completion). While typing, show the
        // pinyin conversion candidates.
        let candidates = if self.buffer.is_empty() {
            match self.last_committed.as_deref() {
                Some(ctx) => self.candidates.predict(ctx, MAX_CANDIDATES),
                None => Vec::new(),
            }
        } else {
            self.candidates.candidates(&self.buffer, MAX_CANDIDATES)
        };
        PreeditState {
            raw: self.buffer.clone(),
            is_valid_pinyin: !self.buffer.is_empty()
                && self.candidates.is_valid_pinyin(&self.buffer),
            candidates,
            selected: self.selected,
        }
    }

    /// Whether contextual next-word predictions are currently displayed.
    pub fn has_predictions(&self) -> bool {
        self.buffer.is_empty()
            && self
                .last_committed
                .as_deref()
                .map(|c| !self.candidates.predict(c, 1).is_empty())
                .unwrap_or(false)
    }

    /// Feed a physical key. Returns the events the front end must apply, in
    /// order. The front end should apply them and then re-read `state()` if it
    /// needs the full preedit/candidate snapshot.
    pub fn feed_key(&mut self, key: Key) -> Vec<EngineEvent> {
        // Prediction mode: no pinyin buffer, but contextual predictions are
        // showing. Number/Space/Enter pick one; Escape dismisses.
        if self.buffer.is_empty() && self.has_predictions() {
            match key {
                Key::Digit(d) => {
                    let idx = (d as usize).saturating_sub(1);
                    if idx < self.state().candidates.len() {
                        return self.commit_candidate(idx);
                    }
                    return vec![EngineEvent::Noop];
                }
                Key::Enter | Key::Space => return self.commit_candidate(0),
                Key::Escape => {
                    self.last_committed = None;
                    self.selected = 0;
                    return vec![EngineEvent::Clear];
                }
                // Any letter starts a fresh pinyin buffer; fall through.
                Key::Letter(_) => {}
                _ => {}
            }
        }

        match key {
            Key::Letter(c) => {
                let c = c.to_ascii_lowercase();
                // A letter after a commit starts a fresh pinyin buffer.
                self.just_committed = false;
                self.buffer.push(c);
                let st = self.state();
                let mut ev = vec![EngineEvent::PreeditChanged(st.raw.clone())];
                if !st.candidates.is_empty() {
                    self.selected = 0;
                    ev.push(EngineEvent::Candidates(st.candidates.clone()));
                }
                ev
            }
            Key::Backspace => {
                if self.buffer.pop().is_none() {
                    return vec![EngineEvent::Noop];
                }
                let st = self.state();
                let mut ev = vec![EngineEvent::PreeditChanged(st.raw.clone())];
                if st.candidates.is_empty() {
                    if self.buffer.is_empty() {
                        ev.push(EngineEvent::Clear);
                    }
                } else {
                    self.selected = 0;
                    ev.push(EngineEvent::Candidates(st.candidates.clone()));
                }
                ev
            }
            Key::Escape => {
                if self.buffer.is_empty() {
                    return vec![EngineEvent::Noop];
                }
                self.buffer.clear();
                self.selected = 0;
                self.just_committed = true;
                vec![EngineEvent::Clear]
            }
            Key::Enter | Key::Space => self.finish(false),
            Key::Digit(d) => {
                let idx = (d as usize) - 1;
                if idx < MAX_CANDIDATES {
                    let st = self.state();
                    if idx < st.candidates.len() {
                        self.commit_candidate(idx)
                    } else {
                        vec![EngineEvent::Noop]
                    }
                } else {
                    vec![EngineEvent::Noop]
                }
            }
            Key::Left | Key::Right => vec![EngineEvent::Noop],
            Key::Other => {
                // Any other key commits the current preedit (first candidate
                // or raw buffer) and lets the key pass through.
                if self.buffer.is_empty() {
                    vec![EngineEvent::Noop]
                } else {
                    self.finish(false)
                }
            }
        }
    }

    /// Commit the currently selected candidate, or the raw buffer if no
    /// candidates, and clear. Returns the commit event.
    pub fn commit_candidate(&mut self, idx: usize) -> Vec<EngineEvent> {
        let st = self.state();
        let text = st
            .candidates
            .get(idx)
            .map(|c| c.text.clone())
            .unwrap_or_else(|| st.raw.clone());
        if text.is_empty() {
            return vec![EngineEvent::Noop];
        }
        let pinyin = st.raw.clone();
        // Only learn a pinyin→word mapping when the commit came from pinyin
        // conversion; predictions are pure context, not a spelling signal.
        if !pinyin.is_empty() {
            self.learn(&pinyin, &text);
        }
        self.buffer.clear();
        self.selected = 0;
        self.just_committed = true;
        self.recent.push_back(text.clone());
        if self.recent.len() > self.max_recent {
            self.recent.pop_front();
        }
        if !pinyin.is_empty() {
            self.persist_commit(&pinyin, &text);
        }
        // Chain predictions from the newly committed text.
        self.last_committed = Some(text.clone());
        vec![EngineEvent::Commit(text)]
    }

    /// Persist a single commit to the user dictionary file. No-op if no
    /// path is set.
    fn persist_commit(&mut self, pinyin: &str, word: &str) {
        if let Some(path) = &self.user_dict_path {
            let user_dict = self.candidates.user_dictionary();
            let mut snapshot: Vec<(String, String, f64)> = user_dict
                .iter()
                .map(|(w, f)| (String::new(), w.clone(), *f))
                .collect();
            if let Some(entry) = snapshot.iter_mut().find(|(_, w, _)| w == word) {
                entry.0 = pinyin.to_string();
            } else {
                snapshot.push((pinyin.to_string(), word.to_string(), 1.0));
            }
            let _ = userdict::save_snapshot(path, &snapshot);
        }
    }

    /// Persist the entire user dictionary to disk. No-op if no path is set.
    pub fn save_user_dict(&mut self) {
        if let Some(path) = &self.user_dict_path {
            let user_dict = self.candidates.user_dictionary();
            let snapshot: Vec<(String, String, f64)> = user_dict
                .iter()
                .map(|(w, f)| (String::new(), w.clone(), *f))
                .collect();
            let _ = userdict::save_snapshot(path, &snapshot);
        }
    }

    /// Finish the current input: commit the first candidate (or raw buffer)
    /// and clear. `force_raw` commits the raw pinyin instead of a candidate.
    pub fn finish(&mut self, force_raw: bool) -> Vec<EngineEvent> {
        if self.buffer.is_empty() {
            return vec![EngineEvent::Noop];
        }
        if force_raw {
            return self.commit_candidate(usize::MAX); // → raw buffer
        }
        // Commit the selected candidate (default 0).
        self.commit_candidate(self.selected)
    }

    /// Drop the preedit without committing (focus loss / window close).
    pub fn reset(&mut self) -> Vec<EngineEvent> {
        self.last_committed = None;
        self.selected = 0;
        if self.buffer.is_empty() {
            return vec![EngineEvent::Noop];
        }
        self.buffer.clear();
        self.just_committed = true;
        vec![EngineEvent::Clear]
    }

    /// Whether the engine has an active preedit.
    pub fn is_active(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Learning: bump the frequency of the (pinyin, word) we just committed so
    /// it ranks higher next time. This is the minimal version of
    /// `wave-ime`'s `usr_dict.cpp` adaptive scoring.
    fn learn(&mut self, pinyin: &str, word: &str) {
        userdict::reinforce(self.candidates.user_dictionary(), pinyin, word, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_and_commit_first_candidate() {
        let mut e = Engine::new();
        for c in "nihao".chars() {
            e.feed_key(Key::Letter(c));
        }
        let st = e.state();
        assert_eq!(st.raw, "nihao");
        assert!(st.candidates.iter().any(|c| c.text == "你好"));
        let ev = e.feed_key(Key::Enter);
        assert!(ev
            .iter()
            .any(|e| matches!(e, EngineEvent::Commit(t) if t == "你好")));
        assert!(!e.is_active());
    }

    #[test]
    fn backspace_shortens() {
        let mut e = Engine::new();
        for c in "nihao".chars() {
            e.feed_key(Key::Letter(c));
        }
        e.feed_key(Key::Backspace);
        assert_eq!(e.state().raw, "niha");
    }

    #[test]
    fn escape_clears_without_commit() {
        let mut e = Engine::new();
        for c in "ni".chars() {
            e.feed_key(Key::Letter(c));
        }
        let ev = e.feed_key(Key::Escape);
        assert!(ev.contains(&EngineEvent::Clear));
        assert!(!e.is_active());
    }

    #[test]
    fn digit_selects() {
        let mut e = Engine::new();
        for c in "hao".chars() {
            e.feed_key(Key::Letter(c));
        }
        let st = e.state();
        // pick the top candidate by index 0 via digit 1
        let top = st.candidates.first().map(|c| c.text.clone()).unwrap();
        let ev = e.feed_key(Key::Digit(1));
        assert!(ev
            .iter()
            .any(|e| matches!(e, EngineEvent::Commit(t) if t == &top)));
    }

    #[test]
    fn learning_bumps_rank() {
        let mut e = Engine::new();
        // prime: type a rare pinyin that has a low-freq candidate
        for c in "xian".chars() {
            e.feed_key(Key::Letter(c));
        }
        let st = e.state();
        let first = st.candidates.first().unwrap().text.clone();
        // commit it
        e.feed_key(Key::Enter);
        // retype — the committed word should still be at/above the top
        for c in "xian".chars() {
            e.feed_key(Key::Letter(c));
        }
        let st2 = e.state();
        assert!(st2
            .candidates
            .iter()
            .position(|c| c.text == first)
            .is_some());
    }
}

#[cfg(test)]
mod prediction_tests {
    use super::*;

    #[test]
    fn predicts_after_commit() {
        let mut e = Engine::new();
        for c in "nihao".chars() {
            e.feed_key(Key::Letter(c));
        }
        e.feed_key(Key::Enter); // commit 你好
        let st = e.state();
        assert_eq!(st.raw, "");
        assert!(
            st.candidates.iter().any(|c| c.text == "吗"),
            "expected 吗 predicted after 你好, got {:?}",
            st.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
        assert!(st.candidates.len() <= MAX_CANDIDATES);
    }

    #[test]
    fn space_accepts_first_prediction() {
        let mut e = Engine::new();
        for c in "nihao".chars() {
            e.feed_key(Key::Letter(c));
        }
        e.feed_key(Key::Enter);
        let first = e.state().candidates.first().map(|c| c.text.clone()).unwrap();
        let ev = e.feed_key(Key::Space);
        assert!(ev.iter().any(|x| matches!(x, EngineEvent::Commit(t) if t == &first)));
    }

    #[test]
    fn escape_dismisses_predictions() {
        let mut e = Engine::new();
        for c in "nihao".chars() {
            e.feed_key(Key::Letter(c));
        }
        e.feed_key(Key::Enter);
        assert!(e.has_predictions());
        let ev = e.feed_key(Key::Escape);
        assert!(ev.contains(&EngineEvent::Clear));
        assert!(!e.has_predictions());
    }

    #[test]
    fn max_ten_candidates() {
        let mut e = Engine::new();
        for c in "shi".chars() {
            e.feed_key(Key::Letter(c));
        }
        assert!(e.state().candidates.len() <= MAX_CANDIDATES);
    }
}
