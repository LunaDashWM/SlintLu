//! Pointer motion, buttons and scrolling.

use slint::LogicalPosition;
use slint::platform::{PointerEventButton, WindowEvent};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, PointerEvent, PointerEventKind, PointerHandler,
};

use crate::core::state::State;

// Linux input event codes (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// Logical pixels Slint scrolls per wheel notch.
pub(crate) const PIXELS_PER_SCROLL_STEP: f64 = 60.0;

fn scroll_delta(axis: &AxisScroll) -> f32 {
    // Wayland scrolls "down" with positive values, Slint the other way around.
    let delta = if axis.value120 != 0 {
        axis.value120 as f64 / 120.0 * PIXELS_PER_SCROLL_STEP
    } else if axis.discrete != 0 {
        axis.discrete as f64 * PIXELS_PER_SCROLL_STEP
    } else {
        axis.absolute
    };
    -delta as f32
}

fn pointer_button(button: u32) -> PointerEventButton {
    match button {
        BTN_LEFT => PointerEventButton::Left,
        BTN_RIGHT => PointerEventButton::Right,
        BTN_MIDDLE => PointerEventButton::Middle,
        BTN_SIDE => PointerEventButton::Back,
        BTN_EXTRA => PointerEventButton::Forward,
        _ => PointerEventButton::Other,
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(window) = self.ctx.find_window(&event.surface) else { continue };
            let position = LogicalPosition::new(event.position.0 as f32, event.position.1 as f32);
            let slint_event = match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(device) = &self.devices.cursor_device {
                        device.set_shape(serial, Shape::Default);
                    }
                    WindowEvent::PointerMoved { position }
                }
                PointerEventKind::Leave { .. } => WindowEvent::PointerExited,
                PointerEventKind::Motion { .. } => WindowEvent::PointerMoved { position },
                PointerEventKind::Press { button, serial, .. } => {
                    self.ctx.clipboard.last_serial.set(serial);
                    WindowEvent::PointerPressed { position, button: pointer_button(button) }
                }
                PointerEventKind::Release { button, .. } => {
                    WindowEvent::PointerReleased { position, button: pointer_button(button) }
                }
                PointerEventKind::Axis { ref horizontal, ref vertical, .. } => {
                    WindowEvent::PointerScrolled {
                        position,
                        delta_x: scroll_delta(horizontal),
                        delta_y: scroll_delta(vertical),
                    }
                }
            };
            window.dispatch(slint_event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(absolute: f64, discrete: i32, value120: i32) -> AxisScroll {
        AxisScroll { absolute, discrete, value120, relative_direction: None, stop: false }
    }

    #[test]
    fn wheel_scrolling_is_inverted_and_scaled() {
        assert_eq!(scroll_delta(&axis(10.0, 1, 120)), -60.0);
        assert_eq!(scroll_delta(&axis(10.0, -1, 0)), 60.0);
        assert_eq!(scroll_delta(&axis(-4.5, 0, 0)), 4.5);
    }

    #[test]
    fn buttons_map_to_slint() {
        assert_eq!(pointer_button(BTN_LEFT), PointerEventButton::Left);
        assert_eq!(pointer_button(BTN_RIGHT), PointerEventButton::Right);
        assert_eq!(pointer_button(BTN_MIDDLE), PointerEventButton::Middle);
        assert_eq!(pointer_button(0x200), PointerEventButton::Other);
    }
}
