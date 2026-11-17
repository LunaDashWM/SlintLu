//! Translation of X events into Slint window events.

use slint::LogicalPosition;
use slint::platform::{PointerEventButton, WindowEvent};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    self, ClientMessageEvent, ConnectionExt as _, EventMask, Mapping, NotifyMode,
};

use super::context::Context;
use super::keymap::Keymap;
use super::window::X11Window;
use crate::input::keys::key_text;
use crate::input::{ModifierKeys, PIXELS_PER_SCROLL_STEP};

/// Keyboard state kept between events.
#[derive(Default)]
pub(crate) struct Input {
    /// Keys held down, to tell auto-repeat (presses without releases) from new presses.
    pressed: Vec<u8>,
    modifier_keys: ModifierKeys,
}

enum Button {
    Pointer(PointerEventButton),
    Scroll { delta_x: f32, delta_y: f32 },
}

fn button(detail: u8) -> Button {
    let step = PIXELS_PER_SCROLL_STEP as f32;
    match detail {
        1 => Button::Pointer(PointerEventButton::Left),
        2 => Button::Pointer(PointerEventButton::Middle),
        3 => Button::Pointer(PointerEventButton::Right),
        // Wheel "buttons": up, down, left, right. Slint scrolls up/left with positive deltas.
        4 => Button::Scroll { delta_x: 0.0, delta_y: step },
        5 => Button::Scroll { delta_x: 0.0, delta_y: -step },
        6 => Button::Scroll { delta_x: step, delta_y: 0.0 },
        7 => Button::Scroll { delta_x: -step, delta_y: 0.0 },
        8 => Button::Pointer(PointerEventButton::Back),
        9 => Button::Pointer(PointerEventButton::Forward),
        _ => Button::Pointer(PointerEventButton::Other),
    }
}

fn position(window: &X11Window, x: i16, y: i16) -> LogicalPosition {
    let scale = window.scale();
    LogicalPosition::new(x as f32 / scale, y as f32 / scale)
}

pub(crate) fn handle(ctx: &Context, input: &mut Input, event: Event) {
    let window = |xid: xproto::Window| ctx.find_window(xid);
    match event {
        Event::Expose(e) if e.count == 0 => {
            if let Some(w) = window(e.window) {
                w.exposed();
            }
        }
        Event::ConfigureNotify(e) => {
            if let Some(w) = window(e.window) {
                w.resized(e.width, e.height);
            }
        }
        Event::MapNotify(e) => {
            if let Some(w) = window(e.window) {
                w.mapped();
            }
        }
        Event::ClientMessage(e) if e.type_ == ctx.atoms.WM_PROTOCOLS && e.format == 32 => {
            let data = e.data.as_data32();
            if data[0] == ctx.atoms.WM_DELETE_WINDOW {
                if let Some(w) = window(e.window) {
                    w.request_close();
                }
            } else if data[0] == ctx.atoms._NET_WM_PING {
                // Answer the window manager's "are you alive" check.
                let reply = ClientMessageEvent { window: ctx.root, ..e };
                let mask = EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT;
                let _ = ctx.conn.send_event(false, ctx.root, mask, reply);
            }
        }
        Event::ButtonPress(e) => {
            let Some(w) = window(e.event) else { return };
            let position = position(&w, e.event_x, e.event_y);
            w.dispatch(match button(e.detail) {
                Button::Pointer(button) => WindowEvent::PointerPressed { position, button },
                Button::Scroll { delta_x, delta_y } => {
                    WindowEvent::PointerScrolled { position, delta_x, delta_y }
                }
            });
        }
        Event::ButtonRelease(e) => {
            let Some(w) = window(e.event) else { return };
            if let Button::Pointer(button) = button(e.detail) {
                let position = position(&w, e.event_x, e.event_y);
                w.dispatch(WindowEvent::PointerReleased { position, button });
            }
        }
        Event::MotionNotify(e) => {
            if let Some(w) = window(e.event) {
                w.dispatch(WindowEvent::PointerMoved { position: position(&w, e.event_x, e.event_y) });
            }
        }
        Event::EnterNotify(e) => {
            if let Some(w) = window(e.event) {
                w.dispatch(WindowEvent::PointerMoved { position: position(&w, e.event_x, e.event_y) });
            }
        }
        // Leaving because of a pointer grab is followed by an ungrab; only real exits count.
        Event::LeaveNotify(e) if e.mode != NotifyMode::GRAB => {
            if let Some(w) = window(e.event) {
                w.dispatch(WindowEvent::PointerExited);
            }
        }
        Event::KeyPress(e) => key(ctx, input, e.event, e.detail, e.state.into(), true),
        Event::KeyRelease(e) => key(ctx, input, e.event, e.detail, e.state.into(), false),
        Event::FocusIn(e) if e.mode != NotifyMode::GRAB && e.mode != NotifyMode::UNGRAB => {
            if let Some(w) = window(e.event) {
                w.dispatch(WindowEvent::WindowActiveChanged(true));
            }
        }
        Event::FocusOut(e) if e.mode != NotifyMode::GRAB && e.mode != NotifyMode::UNGRAB => {
            *input = Input::default();
            if let Some(w) = window(e.event) {
                w.dispatch(WindowEvent::WindowActiveChanged(false));
            }
        }
        Event::MappingNotify(e) if e.request != Mapping::POINTER => match Keymap::load(&ctx.conn) {
            Ok(keymap) => *ctx.keymap.borrow_mut() = keymap,
            Err(e) => log::warn!("cannot reload the keyboard mapping: {e}"),
        },
        Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => super::output::changed(ctx),
        Event::Error(e) => log::warn!("X11 error: {e:?}"),
        _ => {}
    }
}

fn key(ctx: &Context, input: &mut Input, xid: xproto::Window, keycode: u8, state: u16, down: bool) {
    let Some(window) = ctx.find_window(xid) else { return };
    let keysym = ctx.keymap.borrow().keysym(keycode, state);
    let repeat = down && input.pressed.contains(&keycode);
    if down {
        if !repeat {
            input.pressed.push(keycode);
        }
    } else {
        input.pressed.retain(|&k| k != keycode);
    }
    let Some(text) = key_text(keysym, None) else {
        log::trace!("key {keysym:?} produces no text");
        return;
    };
    input.modifier_keys.track(&text, down);
    window.dispatch(match (down, repeat) {
        (true, false) => WindowEvent::KeyPressed { text },
        (true, true) => WindowEvent::KeyPressRepeated { text },
        (false, _) => WindowEvent::KeyReleased { text },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_buttons_scroll() {
        assert!(matches!(button(4), Button::Scroll { delta_y, .. } if delta_y > 0.0));
        assert!(matches!(button(5), Button::Scroll { delta_y, .. } if delta_y < 0.0));
        assert!(matches!(button(6), Button::Scroll { delta_x, .. } if delta_x > 0.0));
        assert!(matches!(button(7), Button::Scroll { delta_x, .. } if delta_x < 0.0));
    }

    #[test]
    fn buttons_map_to_slint() {
        assert!(matches!(button(1), Button::Pointer(PointerEventButton::Left)));
        assert!(matches!(button(2), Button::Pointer(PointerEventButton::Middle)));
        assert!(matches!(button(3), Button::Pointer(PointerEventButton::Right)));
        assert!(matches!(button(8), Button::Pointer(PointerEventButton::Back)));
        assert!(matches!(button(12), Button::Pointer(PointerEventButton::Other)));
    }
}
