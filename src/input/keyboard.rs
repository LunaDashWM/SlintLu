//! Keyboard focus and key events.

use std::rc::{Rc, Weak};

use slint::SharedString;
use slint::platform::{Key, WindowEvent};
use smithay_client_toolkit::reexports::client::protocol::{wl_keyboard, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};

use super::keys::key_text;
use crate::core::state::State;
use crate::window::WaylandWindow;

/// Modifier state as Slint last saw it (Slint derives modifiers from modifier key events).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModifierKeys {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub logo: bool,
}

impl ModifierKeys {
    fn from_xkb(m: &Modifiers) -> Self {
        Self { shift: m.shift, ctrl: m.ctrl, alt: m.alt, logo: m.logo }
    }

    /// Updates the state from a key Slint is about to receive.
    fn track(&mut self, text: &str, pressed: bool) {
        let is = |keys: [Key; 2]| keys.into_iter().any(|k| SharedString::from(k) == text);
        if is([Key::Shift, Key::ShiftR]) {
            self.shift = pressed;
        } else if is([Key::Control, Key::ControlR]) {
            self.ctrl = pressed;
        } else if is([Key::Alt, Key::AltGr]) {
            self.alt = pressed;
        } else if is([Key::Meta, Key::MetaR]) {
            self.logo = pressed;
        }
    }
}

pub(crate) enum KeyKind {
    Press,
    Repeat,
    Release,
}

impl State {
    fn focused_window(&self) -> Option<Rc<WaylandWindow>> {
        self.keyboard_focus.as_ref().and_then(Weak::upgrade)
    }

    pub(crate) fn key_event(&mut self, event: KeyEvent, kind: KeyKind) {
        let Some(window) = self.focused_window() else {
            log::debug!("key event without a focused window: {:?}", event.keysym);
            return;
        };
        let Some(text) = key_text(event.keysym, event.utf8.as_deref()) else { return };
        log::trace!("key {:?} -> {text:?}", event.keysym);
        match kind {
            KeyKind::Press | KeyKind::Repeat => self.modifier_keys.track(&text, true),
            KeyKind::Release => self.modifier_keys.track(&text, false),
        }
        window.dispatch(match kind {
            KeyKind::Press => WindowEvent::KeyPressed { text },
            KeyKind::Repeat => WindowEvent::KeyPressRepeated { text },
            KeyKind::Release => WindowEvent::KeyReleased { text },
        });
    }
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        let window = self.ctx.find_window(surface);
        log::debug!(
            "keyboard focus enters {}",
            if window.is_some() { "a SlintLu window" } else { "an unknown surface" }
        );
        self.keyboard_focus = window.as_ref().map(Rc::downgrade);
        if let Some(window) = window {
            window.dispatch(WindowEvent::WindowActiveChanged(true));
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(window) = self.keyboard_focus.take().and_then(|w| w.upgrade()) {
            window.dispatch(WindowEvent::WindowActiveChanged(false));
        }
        self.modifier_keys = ModifierKeys::default();
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.ctx.clipboard.last_serial.set(serial);
        self.key_event(event, KeyKind::Press);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key_event(event, KeyKind::Repeat);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key_event(event, KeyKind::Release);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        // Physical keyboards also send the modifier keys themselves, but virtual keyboards
        // (on-screen keyboards, remote desktops, automation) may only change the state.
        // Synthesize the key events Slint needs for anything it has not seen.
        let Some(window) = self.focused_window() else { return };
        let wanted = ModifierKeys::from_xkb(&modifiers);
        let seen = self.modifier_keys;
        for (now, before, key) in [
            (wanted.shift, seen.shift, Key::Shift),
            (wanted.ctrl, seen.ctrl, Key::Control),
            (wanted.alt, seen.alt, Key::Alt),
            (wanted.logo, seen.logo, Key::Meta),
        ] {
            if now != before {
                let text: SharedString = key.into();
                window.dispatch(if now {
                    WindowEvent::KeyPressed { text }
                } else {
                    WindowEvent::KeyReleased { text }
                });
            }
        }
        self.modifier_keys = wanted;
    }
}
