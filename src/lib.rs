//! A [Slint](https://slint.dev) platform backend for building Wayland desktop shells.
//!
//! `slintlu-wayland` replaces Slint's default (winit) backend with one that talks to the
//! compositor directly through [smithay-client-toolkit], which gives Slint windows access to
//! shell-only protocols such as `wlr-layer-shell` (bars, docks, wallpapers, overlays,
//! notifications, launchers …) next to ordinary `xdg-shell` toplevel windows.
//!
//! Rendering is done by one of Slint's renderers:
//!
//! | [`RendererKind`] | Renderer | Graphics API |
//! |---|---|---|
//! | `Software` | Slint software renderer | `wl_shm` shared memory, no GPU needed |
//! | `OpenGL` | FemtoVG | OpenGL ES 2/3 through EGL (`libEGL` loaded at runtime) |
//! | `Vulkan` | FemtoVG on wgpu | Vulkan |
//! | `Auto` | first one that works | Vulkan → OpenGL → Software |
//!
//! # Using it from Rust
//!
//! ```no_run
//! use slintlu::{Anchors, LayerConfig, PlatformConfig, SurfaceRole};
//!
//! slint::slint! {
//!     export component Bar inherits Window {
//!         preferred-height: 32px;
//!         Text { text: "hello from a layer surface"; }
//!     }
//! }
//!
//! slintlu::init(PlatformConfig::default()).unwrap();
//! let bar = Bar::new().unwrap();
//! slintlu::window_for(slint::ComponentHandle::window(&bar))
//!     .unwrap()
//!     .set_role(SurfaceRole::Layer(LayerConfig {
//!         anchors: Anchors::TOP_EDGE,
//!         ..LayerConfig::default()
//!     }));
//! slint::ComponentHandle::run(&bar).unwrap();
//! ```
//!
//! [smithay-client-toolkit]: https://docs.rs/smithay-client-toolkit

pub mod audio;
pub mod clipboard;
pub mod compositors;
mod core;
pub mod dbus;
pub mod debug;
pub mod files;
pub mod i18n;
pub mod icons;
pub mod idle;
mod input;
pub mod ipc;
pub mod launcher;
mod output;
mod platform;
mod render;
pub mod security;
mod shell;
pub mod store;
pub mod system;
pub mod theme;
pub mod time;
pub mod util;
mod window;

pub use core::WaylandPlatform;
pub use output::{OutputInfo, on_outputs_changed, outputs};
pub use platform::{PlatformConfig, ShellWindow, init, window_for};
pub use render::RendererKind;
pub use shell::config::{
    Anchors, ExclusiveZone, KeyboardMode, Layer, LayerConfig, Margins, SurfaceRole,
};
pub use window::WaylandWindow;
