//! XIM (X Input Method) native commit via `FnCommitString`.
//!
//! When our IME is registered as an XIM server, the client app creates an
//! XIC (XIM Input Context) connected to us. To commit text, we write to the
//! XIC's `FnCommitString` property, which the client reads and inserts at
//! the cursor. This is the standard X11 IM protocol (same mechanism fcitx5
//! and ibus use to deliver text to apps).

use std::os::raw::{c_int, c_uchar, c_ulong};
use std::sync::OnceLock;

use x11_dl::xlib;

fn xlib() -> Option<&'static xlib::Xlib> {
    static CACHE: OnceLock<Option<xlib::Xlib>> = OnceLock::new();
    let v = CACHE.get_or_init(|| xlib::Xlib::open().ok());
    v.as_ref()
}

/// An open XIM Input Context connection from a client app.
#[derive(Clone)]
pub struct Xic {
    /// The client's XIC XID (window).
    pub xic: c_ulong,
    /// The client's XID (top-level window).
    pub client: c_ulong,
    /// The client's top-level XID.
    pub client_top: c_ulong,
    /// Cached atom IDs for this XIC.
    pub atoms: XimAtoms,
}

#[derive(Clone)]
pub struct XimAtoms {
    pub fn_commit_string: c_ulong,
    pub xim_preedit_string: c_ulong,
    pub xim_reset: c_ulong,
}

impl XimAtoms {
    fn intern(display: *mut xlib::Display, xlib: &xlib::Xlib) -> Self {
        unsafe {
            XimAtoms {
                fn_commit_string: (xlib.XInternAtom)(display, c"IM_COMMIT".as_ptr(), 0),
                xim_preedit_string: (xlib.XInternAtom)(display, c"XIM_PREEDIT_STRING".as_ptr(), 0),
                xim_reset: (xlib.XInternAtom)(display, c"XIM_RESET".as_ptr(), 0),
            }
        }
    }
}

/// Commit UTF-8 text to a client via XIM FnCommitString.
///
/// Writes the `IM_COMMIT` property to the client's XIC, then sends a
/// PropertyNotify event. The client (fcitx5/im module, or GTK/Qt) reads it
/// and inserts the text at the cursor.
///
/// # Safety
/// `display` must be a valid X11 Display.
pub unsafe fn commit_to_xic(
    display: *mut xlib::Display,
    xic: &Xic,
    text: &str,
) -> Result<(), String> {
    let xlib = xlib().ok_or_else(|| "Xlib not available".to_string())?;
    let utf8 = text.as_bytes();
    let len = utf8.len();

    // IM_COMMIT property format:
    //   IME_NAME\0IME_VALUE\0IME_TYPE\0IME_OFFSET\0IME_LENGTH\0
    let mut prop: Vec<u8> = Vec::with_capacity(32 + len);
    prop.extend_from_slice(b"doubao\0");
    prop.extend_from_slice(utf8);
    prop.push(0);
    prop.extend_from_slice(b"utf8\0");
    prop.extend_from_slice(&0i32.to_ne_bytes());
    prop.push(0);
    prop.extend_from_slice(&(len as i32).to_ne_bytes());
    prop.push(0);

    unsafe {
        let atom_string = (xlib.XInternAtom)(display, c"STRING".as_ptr(), 0);
        (xlib.XChangeProperty)(
            display,
            xic.xic,
            xic.atoms.fn_commit_string,
            atom_string,
            8,
            0, // PropModeReplace
            prop.as_ptr(),
            prop.len() as c_int,
        );

        // Send PropertyNotify to the XIC.
        let mut event: xlib::XEvent = std::mem::zeroed();
        event.property.type_ = xlib::PropertyNotify as c_int;
        event.property.window = xic.xic;
        event.property.atom = xic.atoms.fn_commit_string;
        event.property.time = 0;
        event.property.state = 0;

        (xlib.XSendEvent)(display, xic.xic, 0, xlib::PropertyChangeMask, &mut event);
        (xlib.XFlush)(display);
    }

    Ok(())
}

/// Set the preedit (underlined pinyin) visible in the client's in-place window.
///
/// # Safety
/// `display` must be a valid X11 Display.
pub unsafe fn set_preedit(
    display: *mut xlib::Display,
    xic: &Xic,
    text: &str,
    cursor_offset: i32,
) -> Result<(), String> {
    let xlib = xlib().ok_or_else(|| "Xlib not available".to_string())?;
    let utf8 = text.as_bytes();

    let mut prop: Vec<u8> = Vec::with_capacity(16 + utf8.len());
    prop.extend_from_slice(utf8);
    prop.push(0);
    prop.extend_from_slice(&cursor_offset.to_ne_bytes());
    prop.push(0);
    prop.extend_from_slice(&(utf8.len() as i32).to_ne_bytes());
    prop.push(0);

    unsafe {
        let atom_string = (xlib.XInternAtom)(display, c"STRING".as_ptr(), 0);
        (xlib.XChangeProperty)(
            display,
            xic.xic,
            xic.atoms.xim_preedit_string,
            atom_string,
            8,
            0,
            prop.as_ptr(),
            prop.len() as c_int,
        );

        let mut event: xlib::XEvent = std::mem::zeroed();
        event.property.type_ = xlib::PropertyNotify as c_int;
        event.property.window = xic.xic;
        event.property.atom = xic.atoms.xim_preedit_string;
        event.property.time = 0;
        event.property.state = 0;

        (xlib.XSendEvent)(display, xic.xic, 0, xlib::PropertyChangeMask, &mut event);
        (xlib.XFlush)(display);
    }

    Ok(())
}

/// Clear the preedit (e.g. after commit or escape).
///
/// # Safety
/// `display` must be a valid X11 Display.
pub unsafe fn clear_preedit(display: *mut xlib::Display, xic: &Xic) -> Result<(), String> {
    set_preedit(display, xic, "", 0)
}

/// Send XIM_RESET to the client (clears all IM state).
///
/// # Safety
/// `display` must be a valid X11 Display.
pub unsafe fn reset_xic(display: *mut xlib::Display, xic: &Xic) -> Result<(), String> {
    let xlib = xlib().ok_or_else(|| "Xlib not available".to_string())?;
    let zero: c_uchar = 0;
    unsafe {
        let atom_cardinal = (xlib.XInternAtom)(display, c"CARDINAL".as_ptr(), 0);
        (xlib.XChangeProperty)(
            display,
            xic.xic,
            xic.atoms.xim_reset,
            atom_cardinal,
            32,
            0,
            &zero as *const c_uchar,
            1,
        );
        (xlib.XFlush)(display);
    }
    Ok(())
}

/// Parse a raw XIC connection from the XIM_CLIENT_PREFIX property.
///
/// The client stores: XIC_ID\0CLIENT_ID\0CLIENT_TOP_ID\0
///
/// # Safety
/// `display` must be a valid X11 Display.
pub unsafe fn parse_xic_property(
    display: *mut xlib::Display,
    xlib: &xlib::Xlib,
    window: c_ulong,
    prefix_atom: c_ulong,
) -> Option<Xic> {
    let mut actual_type: c_ulong = 0;
    let mut actual_format: c_int = 0;
    let mut n_items: c_ulong = 0;
    let mut bytes_after: c_ulong = 0;
    let mut data: *mut c_uchar = std::ptr::null_mut();

    unsafe {
        let atom_string = (xlib.XInternAtom)(display, c"STRING".as_ptr(), 0);
        let status = (xlib.XGetWindowProperty)(
            display,
            window,
            prefix_atom,
            0,
            1024,
            0,
            atom_string,
            &mut actual_type,
            &mut actual_format,
            &mut n_items,
            &mut bytes_after,
            &mut data,
        );
        if status != 0 || data.is_null() {
            return None;
        }

        let str_bytes = std::slice::from_raw_parts(data, n_items as usize);
        let s = String::from_utf8_lossy(str_bytes);
        if !data.is_null() {
            (xlib.XFree)(data as *mut _);
        }

        let parts: Vec<&str> = s.split('\0').collect();
        if parts.len() < 3 {
            return None;
        }

        let xic_id: c_ulong = parts[0].parse().ok()?;
        let client_id: c_ulong = parts[1].parse().ok()?;
        let client_top: c_ulong = parts[2].parse().ok()?;

        let atoms = XimAtoms::intern(display, xlib);
        Some(Xic {
            xic: xic_id,
            client: client_id,
            client_top,
            atoms,
        })
    }
}
