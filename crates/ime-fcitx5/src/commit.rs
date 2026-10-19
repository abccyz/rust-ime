//! Commit text to the focused application via XTest synthetic key events.

use std::ffi::CString;
use std::sync::OnceLock;

use x11_dl::xlib;
use x11_dl::xtest;

fn xlib() -> Option<&'static xlib::Xlib> {
    static CACHE: OnceLock<Option<xlib::Xlib>> = OnceLock::new();
    let v = CACHE.get_or_init(|| xlib::Xlib::open().ok());
    v.as_ref()
}

fn xtest() -> Option<&'static xtest::Xf86vmode> {
    static CACHE: OnceLock<Option<xtest::Xf86vmode>> = OnceLock::new();
    let v = CACHE.get_or_init(|| xtest::Xf86vmode::open().ok());
    v.as_ref()
}

/// Commit UTF-8 text to the focused window using XTest synthetic key events.
///
/// For each ASCII character, looks up the keysym via `XStringToKeysym`, then
/// sends a press+release pair via `XTestFakeKeyEvent`. Non-ASCII characters
/// (e.g. CJK) have no standard X11 keysym, so they are skipped — the caller
/// should use `commit_utf8_via_clipboard` for full Unicode support.
///
/// # Safety
/// `display` must be a valid, non-null pointer to an X11 `Display` obtained
/// from `XOpenDisplay`.
pub unsafe fn commit_text(display: *mut xlib::Display, text: &str) -> Result<(), String> {
    if display.is_null() {
        return Err("null display".into());
    }
    let xlib = xlib().ok_or_else(|| "Xlib not available".to_string())?;
    let xtest = xtest().ok_or_else(|| "XTest not available".to_string())?;
    for ch in text.chars() {
        if ch as u32 > 0x7f {
            continue;
        }
        let cbuf = CString::new(ch.to_string()).map_err(|_| "NUL in char".to_string())?;
        let keysym = unsafe { (xlib.XStringToKeysym)(cbuf.as_ptr()) };
        if keysym == 0 {
            continue;
        }
        unsafe {
            (xtest.XTestFakeKeyEvent)(display, keysym as u32, 1, 0);
            (xtest.XTestFakeKeyEvent)(display, keysym as u32, 0, 0);
            (xlib.XFlush)(display);
        }
    }
    Ok(())
}
