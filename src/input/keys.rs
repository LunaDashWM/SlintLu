//! Translation of XKB keysyms into the text Slint expects in key events.

use slint::SharedString;
use slint::platform::Key;
use smithay_client_toolkit::seat::keyboard::Keysym;

/// Returns the Slint key text for a key that produced `keysym` and, if the keymap says so,
/// `utf8`, or `None` when the key produces nothing Slint understands (e.g. dead keys).
pub(crate) fn key_text(keysym: Keysym, utf8: Option<&str>) -> Option<SharedString> {
    if let Some(key) = special_key(keysym) {
        return Some(key.into());
    }
    match utf8 {
        // Control characters are produced for e.g. Ctrl+A; Slint wants the plain letter with the
        // modifier tracked separately.
        Some(text) if !text.is_empty() && !text.chars().all(char::is_control) => Some(text.into()),
        _ => keysym.key_char().filter(|c| !c.is_control()).map(|c| c.into()),
    }
}

fn special_key(sym: Keysym) -> Option<Key> {
    Some(match sym {
        Keysym::BackSpace => Key::Backspace,
        Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
        Keysym::Return | Keysym::KP_Enter => Key::Return,
        Keysym::Escape => Key::Escape,
        Keysym::Delete | Keysym::KP_Delete => Key::Delete,
        Keysym::Shift_L => Key::Shift,
        Keysym::Shift_R => Key::ShiftR,
        Keysym::Control_L => Key::Control,
        Keysym::Control_R => Key::ControlR,
        Keysym::Alt_L => Key::Alt,
        Keysym::Alt_R | Keysym::ISO_Level3_Shift => Key::AltGr,
        Keysym::Super_L | Keysym::Meta_L => Key::Meta,
        Keysym::Super_R | Keysym::Meta_R => Key::MetaR,
        Keysym::Caps_Lock => Key::CapsLock,
        Keysym::space => Key::Space,
        Keysym::Up | Keysym::KP_Up => Key::UpArrow,
        Keysym::Down | Keysym::KP_Down => Key::DownArrow,
        Keysym::Left | Keysym::KP_Left => Key::LeftArrow,
        Keysym::Right | Keysym::KP_Right => Key::RightArrow,
        Keysym::Home | Keysym::KP_Home => Key::Home,
        Keysym::End | Keysym::KP_End => Key::End,
        Keysym::Page_Up | Keysym::KP_Page_Up => Key::PageUp,
        Keysym::Page_Down | Keysym::KP_Page_Down => Key::PageDown,
        Keysym::Insert | Keysym::KP_Insert => Key::Insert,
        Keysym::Menu => Key::Menu,
        Keysym::F1 => Key::F1,
        Keysym::F2 => Key::F2,
        Keysym::F3 => Key::F3,
        Keysym::F4 => Key::F4,
        Keysym::F5 => Key::F5,
        Keysym::F6 => Key::F6,
        Keysym::F7 => Key::F7,
        Keysym::F8 => Key::F8,
        Keysym::F9 => Key::F9,
        Keysym::F10 => Key::F10,
        Keysym::F11 => Key::F11,
        Keysym::F12 => Key::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_keys_use_utf8() {
        assert_eq!(key_text(Keysym::a, Some("a")).as_deref(), Some("a"));
        assert_eq!(key_text(Keysym::A, Some("A")).as_deref(), Some("A"));
    }

    #[test]
    fn control_chars_become_plain_letters() {
        // Ctrl+C produces ETX as utf8.
        assert_eq!(key_text(Keysym::c, Some("\u{3}")).as_deref(), Some("c"));
    }

    #[test]
    fn special_keys_map_to_slint_keys() {
        let text: SharedString = Key::Return.into();
        assert_eq!(key_text(Keysym::Return, Some("\r")), Some(text));
        let text: SharedString = Key::Shift.into();
        assert_eq!(key_text(Keysym::Shift_L, None), Some(text));
    }
}
