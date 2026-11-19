//! Seat capability tracking: creates and releases keyboards and pointers as they come and go.

use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::{wl_keyboard, wl_pointer, wl_seat};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;
use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};

use super::keyboard::KeyKind;
use crate::core::state::State;

/// The input devices of the (first) seat.
pub(crate) struct SeatDevices {
    cursor_shape_manager: Option<CursorShapeManager>,
    pub pointer: Option<wl_pointer::WlPointer>,
    /// `wp_cursor_shape_v1` device; compositors without it keep their default cursor.
    pub cursor_device: Option<WpCursorShapeDeviceV1>,
    pub keyboard: Option<wl_keyboard::WlKeyboard>,
}

impl SeatDevices {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<State>) -> Self {
        Self {
            cursor_shape_manager: CursorShapeManager::bind(globals, qh)
                .inspect_err(|e| log::info!("cursor-shape-v1 not available: {e}"))
                .ok(),
            pointer: None,
            cursor_device: None,
            keyboard: None,
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.ctx.seat.borrow_mut().get_or_insert_with(|| seat.clone());
        self.ctx.clipboard.add_seat(&seat, qh);
        self.ctx.clipboard_history.add_seat(&seat, qh);
        self.ctx.idle.set_seat(&seat);
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        // Seats announced in the initial burst may not have gone through `new_seat` yet.
        self.ctx.clipboard.add_seat(&seat, qh);
        self.ctx.clipboard_history.add_seat(&seat, qh);
        self.ctx.idle.set_seat(&seat);
        if capability == Capability::Keyboard && self.devices.keyboard.is_none() {
            let repeat =
                Box::new(|state: &mut State, _: &wl_keyboard::WlKeyboard, event: KeyEvent| {
                    state.key_event(event, KeyKind::Repeat);
                });
            match self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                repeat,
            ) {
                Ok(keyboard) => self.devices.keyboard = Some(keyboard),
                Err(e) => log::warn!("cannot get the keyboard: {e}"),
            }
        }
        if capability == Capability::Pointer && self.devices.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => {
                    self.devices.cursor_device = self
                        .devices
                        .cursor_shape_manager
                        .as_ref()
                        .map(|m| m.get_shape_device(&pointer, qh));
                    self.devices.pointer = Some(pointer);
                }
                Err(e) => log::warn!("cannot get the pointer: {e}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.devices.keyboard.take()
        {
            keyboard.release();
        }
        if capability == Capability::Pointer {
            if let Some(device) = self.devices.cursor_device.take() {
                device.destroy();
            }
            if let Some(pointer) = self.devices.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}
