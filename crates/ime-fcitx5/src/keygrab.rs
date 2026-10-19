//! Global X11 key grab via XRecord + XTest (both in libXtst.so).
//!
//! Thread model — each thread owns its OWN X11 Display connection (X11
//! connections are NOT thread-safe):
//!
//!   grabber thread   : opens DISP A, runs XRecordEnableContext (blocking),
//!                      intercept callback uses DISP A
//!   dispatch thread  : opens DISP B (lazily), used for suppress + inject
//!   main/Tauri thread: owns the display passed into `run()`, used for XIM
//!
//! All grabbed keys flow: intercept (grabber thread) --mpsc--> dispatch thread.

use std::os::raw::{c_char, c_int, c_uchar, c_uint, c_ulong};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};
use std::thread;

use x11_dl::xlib;
use x11_dl::xrecord::{self, XRecordInterceptData};

use crate::x11_keys::keysym_to_key;
use ime_engine::Key;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrabbedKey {
    pub keycode: c_uint,
    pub is_press: bool,
}

/// Resolve an X11 keycode to an `ime_engine::Key`.
///
/// MUST be called from the dispatch thread (uses the auxiliary connection).
/// NEVER call this from the XRecord intercept callback: the grabber thread is
/// blocked inside `XRecordEnableContext` holding its connection, so any
/// round-trip request on that same connection would deadlock.
pub fn resolve_key(keycode: c_uint) -> Key {
    let display = aux_display();
    if display.is_null() {
        return Key::Other;
    }
    let xl = match xlib::Xlib::open() {
        Ok(x) => x,
        Err(_) => return Key::Other,
    };
    let keysym = unsafe { (xl.XKeycodeToKeysym)(display, keycode as c_uchar, 0) };
    keysym_to_key(keysym as u32).unwrap_or(Key::Other)
}

pub struct KeyGrabber {
    active: Arc<AtomicBool>,
    _thread: Option<thread::JoinHandle<()>>,
}

impl KeyGrabber {
    /// Start the global key grabber. Opens its own X11 connection on a
    /// dedicated thread. Returns a handle that stops the grabber on drop.
    pub fn start(tx: Sender<GrabbedKey>) -> Result<Self, String> {
        let active = Arc::new(AtomicBool::new(true));
        let active_clone = Arc::clone(&active);

        // The channel may already be registered via `init_channel`; either
        // way the sender flows into the static slot used by `intercept`.
        let _ = TX.set(tx);

        let handle = thread::Builder::new()
            .name("ime-keygrab".into())
            .spawn(move || Self::record_loop(active_clone))
            .map_err(|e| format!("spawn keygrab thread: {e}"))?;

        Ok(Self {
            active,
            _thread: Some(handle),
        })
    }

    fn record_loop(active: Arc<AtomicBool>) {
        let xl = match xlib::Xlib::open() {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[keygrab] Xlib open failed: {e}");
                return;
            }
        };
        let xtst = match x11_dl::xrecord::Xf86vmode::open() {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[keygrab] XRecord (libXtst) open failed: {e}");
                return;
            }
        };

        // Grabber thread's OWN connection.
        let display = unsafe { (xl.XOpenDisplay)(std::ptr::null()) };
        if display.is_null() {
            eprintln!("[keygrab] XOpenDisplay failed (no DISPLAY?)");
            return;
        }

        let mut major: c_int = 0;
        let mut minor: c_int = 0;
        unsafe {
            (xtst.XRecordQueryVersion)(display, &mut major, &mut minor);
        }
        eprintln!("[keygrab] XRecord version {major}.{minor}");

        let range = unsafe { (xtst.XRecordAllocRange)() };
        if range.is_null() {
            eprintln!("[keygrab] XRecordAllocRange → null");
            return;
        }
        unsafe {
            // Use device_events (raw physical device events), NOT
            // delivered_events. delivered_events only fires for events already
            // delivered to a client (focus/grab dependent); device_events
            // captures every physical key globally.
            (*range).device_events.first = 2; // KeyPress
            (*range).device_events.last = 3;  // KeyRelease
        }

        let mut client_spec: c_ulong = xrecord::XRecordAllClients;
        let mut ranges_arr = [range];
        let context = unsafe {
            (xtst.XRecordCreateContext)(
                display,
                xrecord::XRecordFromServerTime,
                &mut client_spec,
                1,
                ranges_arr.as_mut_ptr(),
                1,
            )
        };
        if context == 0 {
            unsafe { (xl.XFree)(range as *mut _) };
            eprintln!("[keygrab] XRecordCreateContext failed");
            return;
        }

        eprintln!("[keygrab] XRecord enabled — intercepting all keys");

        // XRecordEnableContext BLOCKS and invokes `intercept` for every event
        // until the context is disabled from another thread.
        unsafe {
            (xtst.XRecordEnableContext)(display, context, Some(intercept), std::ptr::null_mut());
        }

        eprintln!("[keygrab] XRecord disabled, cleaning up");
        unsafe {
            (xtst.XRecordDisableContext)(display, context);
            (xtst.XRecordFreeContext)(display, context);
            (xl.XFree)(range as *mut _);
            (xl.XCloseDisplay)(display);
        }
        let _ = active;
    }
}

impl Drop for KeyGrabber {
    fn drop(&mut self) {
        self.active.store(false, Ordering::SeqCst);
        // Note: XRecordEnableContext blocks; we don't force-join here to avoid
        // a hang. The grabber thread dies with the process.
    }
}

// ── Channel static ────────────────────────────────────────────────────────

static TX: OnceLock<Sender<GrabbedKey>> = OnceLock::new();

pub fn init_channel(tx: &Sender<GrabbedKey>) {
    let _ = TX.set(tx.clone());
}

// ── XRecord intercept (runs on the grabber thread) ────────────────────────

unsafe extern "C" fn intercept(_: *mut c_char, data: *mut XRecordInterceptData) {
    if data.is_null() {
        return;
    }
    let data = &*data;
    if data.category != xrecord::XRecordFromServer {
        return;
    }
    if data.data.is_null() || (data.data_len as usize) < 2 {
        return;
    }
    // XRecord delivers a compact 8-byte per-event header, NOT a full XEvent:
    //   byte[0] = type (2 = KeyPress, 3 = KeyRelease)
    //   byte[1] = detail (the keycode for key events)
    //   byte[2..8] = sequence number + server_time
    let bytes = std::slice::from_raw_parts(data.data, data.data_len as usize);
    let type_ = bytes[0];
    let is_press = type_ == 2;
    if type_ != 2 && type_ != 3 {
        return;
    }
    // NOTE: do NOT resolve the keysym here — the grabber thread is blocked
    // inside XRecordEnableContext holding this connection, so a round-trip
    // request would deadlock. The dispatch thread resolves it instead.
    let keycode = bytes[1];

    if let Some(tx) = TX.get() {
        let _ = tx.send(GrabbedKey {
            keycode: keycode as c_uint,
            is_press,
        });
    }
}

// ── Dispatch-thread auxiliary connection (suppress + inject) ──────────────

/// Lazily-opened X11 connection owned by the dispatch thread.
fn aux_display() -> *mut xlib::Display {
    static AUX: OnceLock<usize> = OnceLock::new();
    let addr = *AUX.get_or_init(|| {
        let d = match xlib::Xlib::open() {
            Ok(x) => unsafe { (x.XOpenDisplay)(std::ptr::null()) },
            Err(_) => std::ptr::null_mut(),
        };
        d as usize
    });
    addr as *mut xlib::Display
}

/// Suppress a single key from reaching the focused window.
///
/// Note: XRecord is observe-only; it cannot consume events. To keep a
/// composed key from also reaching the app we briefly grab the keyboard on
/// our auxiliary connection and drain the in-flight event.
pub fn suppress_key_event() {
    let display = aux_display();
    if display.is_null() {
        return;
    }
    unsafe {
        let xl = match xlib::Xlib::open() {
            Ok(x) => x,
            Err(_) => return,
        };
        let xtst = match x11_dl::xtest::Xf86vmode::open() {
            Ok(x) => x,
            Err(_) => return,
        };

        (xtst.XTestGrabControl)(display, 1);
        let root = (xl.XDefaultRootWindow)(display);
        (xl.XGrabKeyboard)(
            display,
            root,
            1,
            xlib::GrabModeAsync as c_int,
            xlib::GrabModeAsync as c_int,
            0,
        );
        let mut n = 0;
        while (xl.XPending)(display) > 0 && n < 16 {
            let mut ev: xlib::XEvent = std::mem::zeroed();
            (xl.XNextEvent)(display, &mut ev);
            n += 1;
        }
        (xl.XUngrabKeyboard)(display, 0);
        (xtst.XTestGrabControl)(display, 0);
        (xl.XFlush)(display);
    }
}

/// Inject ASCII text into the focused window via XTest fake key events.
pub fn inject_text_ascii(text: &str) {
    let display = aux_display();
    if display.is_null() {
        return;
    }
    unsafe {
        let xl = match xlib::Xlib::open() {
            Ok(x) => x,
            Err(_) => return,
        };
        let xtst = match x11_dl::xtest::Xf86vmode::open() {
            Ok(x) => x,
            Err(_) => return,
        };

        for ch in text.chars() {
            let keysym: c_ulong = ch as c_ulong;
            let kc: c_uint = (xl.XKeysymToKeycode)(display, keysym) as c_uint;
            if kc == 0 {
                continue;
            }
            (xtst.XTestFakeKeyEvent)(display, kc, 1, 0);
            (xtst.XTestFakeKeyEvent)(display, kc, 0, 0);
        }
        (xl.XFlush)(display);
    }
}

/// Send `n` BackSpace key presses via XTest (dispatch connection).
/// Used to erase the pinyin letters that leaked into the target app before
/// the Chinese candidate is committed.
pub fn send_backspaces(n: usize) {
    if n == 0 {
        return;
    }
    let display = aux_display();
    if display.is_null() {
        return;
    }
    unsafe {
        let xl = match xlib::Xlib::open() {
            Ok(x) => x,
            Err(_) => return,
        };
        let xtst = match x11_dl::xtest::Xf86vmode::open() {
            Ok(x) => x,
            Err(_) => return,
        };
        let backspace: c_ulong = 0xff08; // XK_BackSpace
        let kc = (xl.XKeysymToKeycode)(display, backspace) as c_uint;
        if kc == 0 {
            return;
        }
        for _ in 0..n {
            (xtst.XTestFakeKeyEvent)(display, kc, 1, 0);
            (xtst.XTestFakeKeyEvent)(display, kc, 0, 0);
        }
        (xl.XFlush)(display);
    }
}

/// Current pointer position in root coordinates (dispatch connection).
/// Used to place the candidate window near the caret.
pub fn pointer_pos() -> Option<(i32, i32)> {    let display = aux_display();
    if display.is_null() {
        return None;
    }
    unsafe {
        let xl = match xlib::Xlib::open() {
            Ok(x) => x,
            Err(_) => return None,
        };
        let root = (xl.XDefaultRootWindow)(display);
        // XQueryPointer(display, w, root_return, child_return,
        //               root_x, root_y, win_x, win_y, mask)
        let mut root_ret: c_ulong = 0;
        let mut child_ret: c_ulong = 0;
        let mut root_x: c_int = 0;
        let mut root_y: c_int = 0;
        let mut win_x: c_int = 0;
        let mut win_y: c_int = 0;
        let mut mask: c_uint = 0;
        let ok = (xl.XQueryPointer)(
            display,
            root,
            &mut root_ret,
            &mut child_ret,
            &mut root_x,
            &mut root_y,
            &mut win_x,
            &mut win_y,
            &mut mask,
        );
        if ok == 1 {
            Some((root_x, root_y))
        } else {
            None
        }
    }
}

// ── Input focus helpers (dispatch connection) ─────────────────────────────
//
// The candidate window must never steal keyboard focus from the app the user
// is typing into. We snapshot the focused window before showing it and
// restore focus right before injecting the committed text.

/// Returns the currently focused X11 window (None if None/PointerRoot).
pub fn get_input_focus() -> Option<c_ulong> {
    let display = aux_display();
    if display.is_null() {
        return None;
    }
    unsafe {
        let xl = match xlib::Xlib::open() {
            Ok(x) => x,
            Err(_) => return None,
        };
        let mut focus: c_ulong = 0;
        let mut revert: c_int = 0;
        (xl.XGetInputFocus)(display, &mut focus, &mut revert);
        // 0 = None, 1 = PointerRoot — not real windows.
        if focus <= 1 {
            None
        } else {
            Some(focus)
        }
    }
}

/// Restore keyboard focus to `win`.
pub fn set_input_focus(win: c_ulong) {
    let display = aux_display();
    if display.is_null() {
        return;
    }
    unsafe {
        let xl = match xlib::Xlib::open() {
            Ok(x) => x,
            Err(_) => return,
        };
        // RevertToPointerRoot = 1, CurrentTime = 0
        (xl.XSetInputFocus)(display, win, 1, 0);
        (xl.XFlush)(display);
    }
}

// ── Built-in UTF-8 text injection (no xdotool dependency) ────────────────
//
// For each character we temporarily remap a spare keycode to the Unicode
// keysym (0x01000000 + codepoint), press/release it via XTEST, then restore
// the mapping. This is the same technique xdotool uses, implemented natively.

fn find_spare_keycode(
    xl: &xlib::Xlib,
    display: *mut xlib::Display,
) -> Option<(c_uchar, c_int)> {
    unsafe {
        let mut min: c_int = 0;
        let mut max: c_int = 0;
        (xl.XDisplayKeycodes)(display, &mut min, &mut max);
        if min <= 0 || max <= min {
            return None;
        }
        let count = max - min + 1;
        let mut per: c_int = 0;
        let map = (xl.XGetKeyboardMapping)(display, min as c_uchar, count, &mut per);
        if map.is_null() || per <= 0 {
            return None;
        }
        let mut spare = None;
        for i in 0..count {
            let base = (i * per) as usize;
            let mut empty = true;
            for j in 0..per as usize {
                if *map.add(base + j) != 0 {
                    empty = false;
                    break;
                }
            }
            // Prefer a keycode near the high end (less likely to be used).
            if empty {
                spare = Some((min + i) as c_uchar);
                if (min + i) >= max - 8 {
                    break;
                }
            }
        }
        (xl.XFree)(map as *mut _);
        spare.map(|kc| (kc, per))
    }
}

/// Inject arbitrary UTF-8 text into the focused window via XTEST.
///
/// Works for ASCII and CJK; falls back to the direct-keysym path when no
/// spare keycode is available.
pub fn inject_text(text: &str) {
    if text.is_empty() {
        return;
    }
    let display = aux_display();
    if display.is_null() {
        return;
    }
    unsafe {
        let xl = match xlib::Xlib::open() {
            Ok(x) => x,
            Err(_) => return,
        };
        let xtst = match x11_dl::xtest::Xf86vmode::open() {
            Ok(x) => x,
            Err(_) => return,
        };

        let spare = find_spare_keycode(&xl, display);
        let Some((spare_kc, per)) = spare else {
            // No spare keycode: fall back to direct keysyms (ASCII only).
            for ch in text.chars() {
                let keysym = ch as c_ulong;
                let kc = (xl.XKeysymToKeycode)(display, keysym) as c_uint;
                if kc == 0 {
                    continue;
                }
                (xtst.XTestFakeKeyEvent)(display, kc, 1, 0);
                (xtst.XTestFakeKeyEvent)(display, kc, 0, 0);
            }
            (xl.XSync)(display, 0);
            return;
        };

        for ch in text.chars() {
            let cp = ch as u32;
            let keysym: c_ulong = if cp < 0x100 {
                cp as c_ulong
            } else {
                0x0100_0000 + cp as c_ulong
            };
            let syms: [c_ulong; 2] = [keysym, keysym];
            (xl.XChangeKeyboardMapping)(
                display,
                spare_kc as c_int,
                per,
                syms.as_ptr() as *mut c_ulong,
                1,
            );
            (xl.XSync)(display, 0);
            // Give clients a moment to pick up the new mapping before the key
            // event; without this the first character is often lost.
            std::thread::sleep(std::time::Duration::from_millis(8));

            (xtst.XTestFakeKeyEvent)(display, spare_kc as c_uint, 1, 0);
            (xtst.XTestFakeKeyEvent)(display, spare_kc as c_uint, 0, 0);
            (xl.XSync)(display, 0);
            std::thread::sleep(std::time::Duration::from_millis(4));
        }

        // Restore the spare keycode to NoSymbol.
        let none: [c_ulong; 2] = [0, 0];
        (xl.XChangeKeyboardMapping)(
            display,
            spare_kc as c_int,
            per,
            none.as_ptr() as *mut c_ulong,
            1,
        );
        (xl.XSync)(display, 0);
    }
}
