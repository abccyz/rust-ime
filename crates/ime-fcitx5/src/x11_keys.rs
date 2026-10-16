//! X11 keysym → `ime_engine::Key` conversion.

use ime_engine::Key;

/// Convert an X11 keysym to an `ime_engine::Key`.
///
/// The X11 keysym constants for letters are `0x61`..=`0x7a` (a-z), digits
/// `0x31`..=`0x39` (1-9), and special keys have fixed values defined in
/// X11/keysymdef.h.
pub fn keysym_to_key(keysym: u32) -> Option<Key> {
    // Letters a-z: keysym 0x61..=0x7a
    if (0x61..=0x7a).contains(&keysym) {
        let ch = (keysym as u8) as char;
        return Some(Key::Letter(ch));
    }
    // Digits 1-9: keysym 0x31..=0x39
    if (0x31..=0x39).contains(&keysym) {
        let d = (keysym - 0x30) as u8;
        return Some(Key::Digit(d));
    }
    // Special keys (from X11/keysymdef.h)
    match keysym {
        0xff08 => Some(Key::Backspace), // XK_BackSpace
        0xff0d => Some(Key::Enter),     // XK_Return
        0x0020 => Some(Key::Space),     // XK_space
        0xff1b => Some(Key::Escape),    // XK_Escape
        0xff51 => Some(Key::Left),      // XK_Left
        0xff53 => Some(Key::Right),     // XK_Right
        _ => None,                      // pass-through (Key::Other at the engine level)
    }
}
