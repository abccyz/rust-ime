//! `ime-fcitx5` — X11 IM framework integration for the doubao Linux IME.
//!
//! Provides:
//! - Global X11 key grab (XTest/XRecord) for capturing keystrokes in any app
//! - XIM registration: listens for client XIC connections, delivers
//!   preedit + commit via the standard XIM property protocol
//! - Fallback commit-to-focus via XTest synthetic key events
//! - Cursor position tracking for candidate window placement
//! - `ImeServer` wrapper around `Engine` with thread-safe key routing
//!
//! The Tauri app (`app/src/main.rs`) owns the X11 display and calls into
//! this crate for key handling, XIM commit, and cursor positioning.

pub mod commit;
pub mod cursor;
pub mod keygrab;
pub mod x11_keys;
pub mod xim;

use std::os::raw::c_ulong;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

use ime_engine::{Engine, EngineEvent, Key, PreeditState};
use x11_dl::xlib;

use xim::Xic;

/// Whether the IME is active (focused input context).
static INIT: AtomicBool = AtomicBool::new(false);

/// Thread-safe IME server: owns the engine + connected XICs.
pub struct ImeServer {
    engine: Mutex<Engine>,
    /// Connected XIM Input Contexts (one per client app).
    xics: RwLock<Vec<Xic>>,
}

impl Default for ImeServer {
    fn default() -> Self {
        Self::new()
    }
}

impl ImeServer {
    pub fn new() -> Self {
        Self {
            engine: Mutex::new(Engine::new()),
            xics: RwLock::new(Vec::new()),
        }
    }

    pub fn with_user_dict(path: &std::path::Path) -> Self {
        Self {
            engine: Mutex::new(Engine::with_user_dict(path)),
            xics: RwLock::new(Vec::new()),
        }
    }

    // ── Key handling ────────────────────────────────────────────────

    /// Handle a physical key. Returns the engine events the front end
    /// must apply (preedit change, commit, candidate update).
    pub fn on_key(&self, key: Key) -> Vec<EngineEvent> {
        let mut e = self.engine.lock().unwrap();
        e.feed_key(key)
    }

    /// Commit a candidate by index.
    pub fn commit_candidate(&self, idx: usize) -> Vec<EngineEvent> {
        let mut e = self.engine.lock().unwrap();
        e.commit_candidate(idx)
    }

    /// Reset the preedit (Escape / focus loss).
    pub fn reset(&self) -> Vec<EngineEvent> {
        let mut e = self.engine.lock().unwrap();
        e.reset()
    }

    // ── XIM connection management ───────────────────────────────────

    /// Register a new XIC connection from a client app.
    pub fn add_xic(&self, xic: Xic) {
        let mut xics = self.xics.write().unwrap();
        xics.push(xic);
    }

    /// Remove a disconnected XIC.
    pub fn remove_xic(&self, xic_id: c_ulong) {
        let mut xics = self.xics.write().unwrap();
        xics.retain(|x| x.xic != xic_id);
    }

    /// Get the last (most recently focused) XIC — the one we commit to.
    pub fn active_xic(&self) -> Option<Xic> {
        let xics = self.xics.read().unwrap();
        xics.last().cloned()
    }

    /// Number of connected clients.
    pub fn xic_count(&self) -> usize {
        self.xics.read().unwrap().len()
    }

    // ── Engine state access ─────────────────────────────────────────

    pub fn state(&self) -> PreeditState {
        let e = self.engine.lock().unwrap();
        e.state()
    }

    pub fn is_active(&self) -> bool {
        let e = self.engine.lock().unwrap();
        e.is_active()
    }

    /// Apply runtime settings from the settings window.
    pub fn apply_settings(
        &self,
        max_candidates: usize,
        prediction: bool,
        learn: bool,
        fuzzy: bool,
        chinese_punctuation: bool,
    ) {
        let mut e = self.engine.lock().unwrap();
        e.set_max_candidates(max_candidates);
        e.set_prediction_enabled(prediction);
        e.set_learn_enabled(learn);
        e.set_fuzzy(fuzzy);
        e.set_chinese_punctuation(chinese_punctuation);
    }

    pub fn chinese_punctuation_enabled(&self) -> bool {
        self.engine.lock().unwrap().chinese_punctuation()
    }

    /// (system words, user words).
    pub fn dict_stats(&self) -> (usize, usize) {
        let e = self.engine.lock().unwrap();
        e.dict_stats()
    }

    /// Drop all learned user entries and persist.
    pub fn clear_user_dict(&self) {
        let mut e = self.engine.lock().unwrap();
        e.clear_user_dict();
    }

    /// Learned user entries as (word, frequency), highest first.
    pub fn user_words(&self, limit: usize) -> Vec<(String, f64)> {
        let e = self.engine.lock().unwrap();
        e.user_words(limit)
    }

    // ── Activate / deactivate ───────────────────────────────────────

    pub fn on_activate(&self) {
        INIT.store(true, Ordering::SeqCst);
    }

    pub fn on_deactivate(&self) {
        let mut e = self.engine.lock().unwrap();
        e.reset();
        INIT.store(false, Ordering::SeqCst);
    }

    pub fn is_initialised() -> bool {
        INIT.load(Ordering::SeqCst)
    }

    // ── XIM delivery ────────────────────────────────────────────────

    /// Commit text to the active client via XIM FnCommitString.
    /// Falls back to XTest if no XIC is connected.
    ///
    /// # Safety
    /// `display` must be a valid X11 Display.
    pub unsafe fn commit_to_client(
        &self,
        display: *mut xlib::Display,
        text: &str,
    ) -> Result<(), String> {
        let xic = self
            .active_xic()
            .ok_or_else(|| "no active XIC for commit".to_string())?;
        xim::commit_to_xic(display, &xic, text)
    }

    /// Update the preedit display in the active client.
    ///
    /// # Safety
    /// `display` must be a valid X11 Display.
    pub unsafe fn update_preedit(
        &self,
        display: *mut xlib::Display,
        text: &str,
        cursor_offset: i32,
    ) -> Result<(), String> {
        let xic = match self.active_xic() {
            Some(x) => x,
            None => return Ok(()),
        };
        xim::set_preedit(display, &xic, text, cursor_offset)
    }

    /// Clear the preedit in the active client.
    ///
    /// # Safety
    /// `display` must be a valid X11 Display.
    pub unsafe fn clear_preedit(&self, display: *mut xlib::Display) -> Result<(), String> {
        let xic = match self.active_xic() {
            Some(x) => x,
            None => return Ok(()),
        };
        xim::clear_preedit(display, &xic)
    }
}

// ── Backward-compat alias ───────────────────────────────────────────
pub type Fcitx5Adapter = ImeServer;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_routes_key() {
        let a = ImeServer::new();
        a.on_activate();
        for c in "ni".chars() {
            a.on_key(Key::Letter(c));
        }
        let ev = a.on_key(Key::Enter);
        assert!(ev.iter().any(|e| matches!(e, EngineEvent::Commit(_))));
        a.on_deactivate();
        assert!(!ImeServer::is_initialised());
    }

    #[test]
    fn keysym_conversion() {
        assert!(matches!(
            x11_keys::keysym_to_key(0x6e),
            Some(Key::Letter('n'))
        ));
        assert!(matches!(
            x11_keys::keysym_to_key(0x69),
            Some(Key::Letter('i'))
        ));
        assert!(matches!(
            x11_keys::keysym_to_key(0xff08),
            Some(Key::Backspace)
        ));
        assert!(matches!(x11_keys::keysym_to_key(0xff0d), Some(Key::Enter)));
        assert!(matches!(x11_keys::keysym_to_key(0x31), Some(Key::Digit(1))));
        assert!(x11_keys::keysym_to_key(0xffe1).is_none());
    }

    #[test]
    fn xic_management() {
        let s = ImeServer::new();
        assert_eq!(s.xic_count(), 0);
        assert!(s.active_xic().is_none());
    }
}
