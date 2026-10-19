//! X11 cursor position for candidate window placement.

use std::os::raw::{c_int, c_uint};
use std::sync::OnceLock;

use x11_dl::xlib;

fn xlib() -> Option<&'static xlib::Xlib> {
    static CACHE: OnceLock<Option<xlib::Xlib>> = OnceLock::new();
    let v = CACHE.get_or_init(|| xlib::Xlib::open().ok());
    v.as_ref()
}

/// Get the current pointer position in root window coordinates.
///
/// Returns `(x, y)` in physical pixels, or `None` if the display is invalid
/// or the XQueryPointer call fails.
///
/// # Safety
/// `display` must be a valid, non-null pointer to an X11 `Display` obtained
/// from `XOpenDisplay`.
pub unsafe fn get_cursor_pos(display: *mut xlib::Display) -> Option<(i32, i32)> {
    if display.is_null() {
        return None;
    }
    let xlib = xlib()?;
    // XQueryPointer(display, w, root_return, child_return,
    //               root_x, root_y, win_x, win_y, mask)
    let root = unsafe { (xlib.XDefaultRootWindow)(display) };
    let mut root_ret: std::os::raw::c_ulong = 0;
    let mut child_ret: std::os::raw::c_ulong = 0;
    let mut root_x: c_int = 0;
    let mut root_y: c_int = 0;
    let mut win_x: c_int = 0;
    let mut win_y: c_int = 0;
    let mut mask: c_uint = 0;
    unsafe {
        let result = (xlib.XQueryPointer)(
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
        if result == 1 {
            Some((root_x as i32, root_y as i32))
        } else {
            None
        }
    }
}
