//! Seat input: keyboard, pointer and cursor handling.

mod keyboard;
pub(crate) mod keys;
mod pointer;
mod seat;

pub(crate) use keyboard::ModifierKeys;
pub(crate) use pointer::PIXELS_PER_SCROLL_STEP;
pub(crate) use seat::SeatDevices;
