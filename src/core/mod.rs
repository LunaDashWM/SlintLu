//! The heart of the backend: the Slint platform, the shared Wayland context and the event loop.

mod compositor;
pub(crate) mod context;
mod platform;
mod proxy;
pub(crate) mod state;

pub use platform::WaylandPlatform;
