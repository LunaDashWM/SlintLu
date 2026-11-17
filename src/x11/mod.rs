//! The X11 backend, for Xorg sessions where no Wayland compositor is running.
//!
//! X11 has no shell protocols: windows are ordinary X windows described to the window manager
//! with ICCCM and EWMH properties. Monitors come from RandR, keyboard input from the core
//! keyboard mapping, and frames are drawn by the software renderer and uploaded with
//! `PutImage`. Layer surfaces are shown as normal windows for now.

mod atoms;
pub(crate) mod context;
mod events;
mod keymap;
mod output;
mod platform;
mod render;
mod window;

pub use platform::X11Platform;
pub use window::X11Window;
