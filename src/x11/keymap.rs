//! Keycode to keysym translation from the core keyboard mapping (X11 protocol, section 5).
//!
//! This covers the first two groups and the Shift, Lock and NumLock rules, which is what X
//! clients without XKB keymaps have always done. Dead keys, compose sequences and input
//! methods are not supported.

use smithay_client_toolkit::seat::keyboard::Keysym;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::ConnectionExt as _;

const SHIFT: u16 = 1 << 0;
const LOCK: u16 = 1 << 1;

pub(crate) struct Keymap {
    min_keycode: u8,
    per_keycode: usize,
    keysyms: Vec<u32>,
    /// Modifier masks bound to the Num_Lock and Mode_switch keys (0 if unbound).
    num_lock: u16,
    mode_switch: u16,
}

impl Keymap {
    pub fn load(conn: &impl Connection) -> Result<Self, ReplyError> {
        let setup = conn.setup();
        let (min_keycode, max_keycode) = (setup.min_keycode, setup.max_keycode);
        let mapping =
            conn.get_keyboard_mapping(min_keycode, max_keycode - min_keycode + 1)?.reply()?;
        let mut keymap = Self {
            min_keycode,
            per_keycode: mapping.keysyms_per_keycode as usize,
            keysyms: mapping.keysyms,
            num_lock: 0,
            mode_switch: 0,
        };

        let modifiers = conn.get_modifier_mapping()?.reply()?;
        let per_modifier = modifiers.keycodes_per_modifier() as usize;
        for (index, keycodes) in modifiers.keycodes.chunks(per_modifier.max(1)).enumerate() {
            let mask = 1u16 << index;
            for &keycode in keycodes {
                let syms = keymap.syms(keycode);
                let num_lock = syms.contains(&Keysym::Num_Lock.raw());
                let mode_switch = syms.contains(&Keysym::Mode_switch.raw());
                if num_lock {
                    keymap.num_lock |= mask;
                }
                if mode_switch {
                    keymap.mode_switch |= mask;
                }
            }
        }
        Ok(keymap)
    }

    fn syms(&self, keycode: u8) -> &[u32] {
        let Some(index) = keycode.checked_sub(self.min_keycode) else { return &[] };
        let start = index as usize * self.per_keycode;
        self.keysyms.get(start..start + self.per_keycode).unwrap_or(&[])
    }

    /// The keysym a key press produces with the modifier and group `state` of its event.
    pub fn keysym(&self, keycode: u8, state: impl Into<u16>) -> Keysym {
        let state = state.into();
        // XKB reports the effective group in bits 13-14; Mode_switch selects the second group
        // on servers without XKB.
        let mut group = ((state >> 13) & 3) as usize;
        if group == 0 && state & self.mode_switch != 0 {
            group = 1;
        }
        select(self.syms(keycode), group, state, self.num_lock)
    }
}

/// Picks the keysym from one key's list following the core protocol rules.
fn select(syms: &[u32], group: usize, state: u16, num_lock: u16) -> Keysym {
    let at = |i: usize| syms.get(i).copied().map(Keysym::new).unwrap_or(Keysym::NoSymbol);
    let pair = |g: usize| (at(2 * g), at(2 * g + 1));
    let (mut first, mut second) = pair(group);
    if first == Keysym::NoSymbol && second == Keysym::NoSymbol {
        (first, second) = pair(0);
    }
    if second == Keysym::NoSymbol {
        (first, second) = (lower(first), upper(first));
    }

    let shift = state & SHIFT != 0;
    let caps = state & LOCK != 0;
    if num_lock != 0 && state & num_lock != 0 && second.is_keypad_key() {
        return if shift { first } else { second };
    }
    match (shift, caps) {
        (false, false) => first,
        (true, false) => second,
        (false, true) => upper(first),
        (true, true) => upper(second),
    }
}

fn convert_case(sym: Keysym, convert: impl Fn(char) -> Option<char>) -> Keysym {
    sym.key_char().and_then(convert).map(Keysym::from_char).unwrap_or(sym)
}

fn single(mut chars: impl Iterator<Item = char>) -> Option<char> {
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

fn upper(sym: Keysym) -> Keysym {
    convert_case(sym, |c| single(c.to_uppercase()))
}

fn lower(sym: Keysym) -> Keysym {
    convert_case(sym, |c| single(c.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NUM_LOCK: u16 = 1 << 4; // Mod2, as on most servers

    fn raw(syms: &[Keysym]) -> Vec<u32> {
        syms.iter().map(|s| s.raw()).collect()
    }

    #[test]
    fn letters_follow_shift_and_caps_lock() {
        // Letter keys usually list only the lowercase keysym.
        let a = raw(&[Keysym::a]);
        assert_eq!(select(&a, 0, 0, NUM_LOCK), Keysym::a);
        assert_eq!(select(&a, 0, SHIFT, NUM_LOCK), Keysym::A);
        assert_eq!(select(&a, 0, LOCK, NUM_LOCK), Keysym::A);
    }

    #[test]
    fn symbols_ignore_caps_lock() {
        let one = raw(&[Keysym::_1, Keysym::exclam]);
        assert_eq!(select(&one, 0, 0, NUM_LOCK), Keysym::_1);
        assert_eq!(select(&one, 0, SHIFT, NUM_LOCK), Keysym::exclam);
        assert_eq!(select(&one, 0, LOCK, NUM_LOCK), Keysym::_1);
    }

    #[test]
    fn num_lock_switches_keypad_keys() {
        let kp = raw(&[Keysym::KP_End, Keysym::KP_1]);
        assert_eq!(select(&kp, 0, 0, NUM_LOCK), Keysym::KP_End);
        assert_eq!(select(&kp, 0, NUM_LOCK, NUM_LOCK), Keysym::KP_1);
        assert_eq!(select(&kp, 0, NUM_LOCK | SHIFT, NUM_LOCK), Keysym::KP_End);
    }

    #[test]
    fn second_group_falls_back_to_the_first() {
        let key = raw(&[Keysym::q, Keysym::Q, Keysym::Cyrillic_shorti, Keysym::Cyrillic_SHORTI]);
        assert_eq!(select(&key, 1, 0, NUM_LOCK), Keysym::Cyrillic_shorti);
        assert_eq!(select(&key, 1, SHIFT, NUM_LOCK), Keysym::Cyrillic_SHORTI);
        let ret = raw(&[Keysym::Return]);
        assert_eq!(select(&ret, 1, 0, NUM_LOCK), Keysym::Return);
    }

    #[test]
    fn unmapped_keys_produce_nothing() {
        assert_eq!(select(&[], 0, 0, NUM_LOCK), Keysym::NoSymbol);
    }
}
