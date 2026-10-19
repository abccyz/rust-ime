//! C ABI for embedding `ime-engine` in a fcitx5 C++ addon.
//!
//! The fcitx5 addon ABI is C++-only, so a thin C++ shim links against this
//! `staticlib` and forwards key events / candidate queries here.
//!
//! All functions are `extern "C"` and memory-safe at the boundary:
//! * caller-owned buffers are written with UTF-8, NUL-terminated, truncated to
//!   `cap`;
//! * the returned length excludes the NUL (or is the truncated length).

use std::ffi::c_char;
use std::sync::Mutex;

use crate::{Engine, EngineEvent, Key};

/// Opaque engine handle.
pub struct RustIme {
    engine: Mutex<Engine>,
    /// Commits produced by the engine, drained by the C++ layer.
    pending: Mutex<Vec<String>>,
}

/// Key kinds shared with the C++ shim.
pub const KEY_LETTER: i32 = 0;
pub const KEY_DIGIT: i32 = 1;
pub const KEY_BACKSPACE: i32 = 2;
pub const KEY_ENTER: i32 = 3;
pub const KEY_SPACE: i32 = 4;
pub const KEY_ESCAPE: i32 = 5;
pub const KEY_LEFT: i32 = 6;
pub const KEY_RIGHT: i32 = 7;
pub const KEY_OTHER: i32 = 8;
pub const KEY_PUNCT: i32 = 9;

fn map_key(kind: i32, cp: u32) -> Key {
    match kind {
        KEY_LETTER => char::from_u32(cp).map_or(Key::Other, Key::Letter),
        KEY_DIGIT => {
            let d = cp.clamp(1, 9) as u8;
            Key::Digit(d)
        }
        KEY_BACKSPACE => Key::Backspace,
        KEY_ENTER => Key::Enter,
        KEY_SPACE => Key::Space,
        KEY_ESCAPE => Key::Escape,
        KEY_LEFT => Key::Left,
        KEY_RIGHT => Key::Right,
        KEY_PUNCT => char::from_u32(cp).map_or(Key::Other, Key::Punct),
        _ => Key::Other,
    }
}

fn write_buf(s: &str, buf: *mut c_char, cap: i32) -> i32 {
    if buf.is_null() || cap <= 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let max = (cap as usize).saturating_sub(1).min(bytes.len());
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, max);
        *buf.add(max) = 0;
    }
    max as i32
}

/// Create a new engine handle.
#[no_mangle]
pub extern "C" fn rust_ime_new() -> *mut RustIme {
    Box::into_raw(Box::new(RustIme {
        engine: Mutex::new(Engine::new()),
        pending: Mutex::new(Vec::new()),
    }))
}

/// Destroy an engine handle.
///
/// # Safety
/// `h` must come from [`rust_ime_new`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_free(h: *mut RustIme) {
    if !h.is_null() {
        drop(Box::from_raw(h));
    }
}

/// Feed a key. Returns 1 if the IME consumed it, 0 to let fcitx5 pass it on.
///
/// # Safety
/// `h` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_key(h: *mut RustIme, kind: i32, cp: u32) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    let key = map_key(kind, cp);
    let events = {
        let mut engine = ime.engine.lock().unwrap();
        engine.feed_key(key)
    };
    let mut handled = false;
    let mut commits = ime.pending.lock().unwrap();
    for ev in events {
        match ev {
            EngineEvent::Commit(t) => {
                commits.push(t);
                handled = true;
            }
            EngineEvent::PreeditChanged(_) | EngineEvent::Candidates(_) => handled = true,
            EngineEvent::Clear => handled = true,
            EngineEvent::Noop => {}
        }
    }
    i32::from(handled)
}

/// Apply runtime settings. Each flag is 1/0.
///
/// # Safety
/// `h` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_configure(
    h: *mut RustIme,
    max_candidates: i32,
    prediction: i32,
    learn: i32,
    fuzzy: i32,
    chinese_punctuation: i32,
) {
    let Some(ime) = h.as_ref() else { return };
    let mut engine = ime.engine.lock().unwrap();
    engine.set_max_candidates(max_candidates.max(1) as usize);
    engine.set_prediction_enabled(prediction != 0);
    engine.set_learn_enabled(learn != 0);
    engine.set_fuzzy(fuzzy != 0);
    engine.set_chinese_punctuation(chinese_punctuation != 0);
}

/// Current candidate selection index.
///
/// # Safety
/// `h` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_selected(h: *mut RustIme) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    ime.engine.lock().unwrap().selected() as i32
}

/// Move the candidate selection by `delta`. Returns 1 if it changed.
///
/// # Safety
/// `h` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_move_selection(h: *mut RustIme, delta: i32) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    let events = {
        let mut engine = ime.engine.lock().unwrap();
        engine.move_selection(delta as isize)
    };
    i32::from(events.iter().any(|e| matches!(e, EngineEvent::Candidates(_))))
}

/// Whether a preedit is active.
///
/// # Safety
/// `h` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_active(h: *mut RustIme) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    let engine = ime.engine.lock().unwrap();
    i32::from(engine.is_active())
}

/// Copy the current preedit (raw pinyin) into `buf`. Returns bytes written.
///
/// # Safety
/// `h` valid, `buf` writable for `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_preedit(h: *mut RustIme, buf: *mut c_char, cap: i32) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    let engine = ime.engine.lock().unwrap();
    write_buf(&engine.state().raw, buf, cap)
}

/// Number of candidates for the current preedit.
///
/// # Safety
/// `h` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_candidate_count(h: *mut RustIme) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    let engine = ime.engine.lock().unwrap();
    engine.state().candidates.len() as i32
}

/// Copy candidate `idx` into `buf`. Returns bytes written (0 if out of range).
///
/// # Safety
/// `h` valid, `buf` writable for `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_candidate(
    h: *mut RustIme,
    idx: i32,
    buf: *mut c_char,
    cap: i32,
) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    let engine = ime.engine.lock().unwrap();
    match engine.state().candidates.get(idx.max(0) as usize) {
        Some(c) => write_buf(&c.text, buf, cap),
        None => 0,
    }
}

/// Commit candidate `idx` (queues the text). Returns 1 if something was queued.
///
/// # Safety
/// `h` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_commit_candidate(h: *mut RustIme, idx: i32) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    let events = {
        let mut engine = ime.engine.lock().unwrap();
        engine.commit_candidate(idx.max(0) as usize)
    };
    let mut queued = false;
    let mut commits = ime.pending.lock().unwrap();
    for ev in events {
        if let EngineEvent::Commit(t) = ev {
            commits.push(t);
            queued = true;
        }
    }
    i32::from(queued)
}

/// Drain the next pending commit into `buf`. Returns bytes written, 0 if none.
///
/// # Safety
/// `h` valid, `buf` writable for `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_take_commit(
    h: *mut RustIme,
    buf: *mut c_char,
    cap: i32,
) -> i32 {
    let Some(ime) = h.as_ref() else { return 0 };
    let mut commits = ime.pending.lock().unwrap();
    if commits.is_empty() {
        return 0;
    }
    let text = commits.remove(0);
    write_buf(&text, buf, cap)
}

/// Drop the preedit without committing.
///
/// # Safety
/// `h` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn rust_ime_reset(h: *mut RustIme) {
    let Some(ime) = h.as_ref() else { return };
    let mut engine = ime.engine.lock().unwrap();
    let _ = engine.reset();
    ime.pending.lock().unwrap().clear();
}
