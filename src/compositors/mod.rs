//! Knowledge about specific compositors: detection and workspace / window IPC.

mod detect;
mod hyprland;
mod lunadash;
mod model;
mod niri;
mod sway;

use std::io;
use std::sync::Arc;

pub use detect::Compositor;
pub use lunadash::LunaDash;
pub use model::{
    Action, CompositorRequest, DesktopState, Shortcut, SwitcherItem, SwitcherState,
    WallpaperEntry, Window, Workspace,
};

/// A compositor's IPC connection.
pub trait Backend: Send + Sync {
    /// Blocks while connected, calling `on_state` with a full snapshot after every change.
    fn watch(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()>;
    /// Asks the compositor to do something.
    fn perform(&self, action: &Action) -> io::Result<()>;
    /// Sends a compositor-specific command (LunaDash control methods such as
    /// `launch-default terminal`) and returns its raw reply.
    fn command(&self, method: &str, value: &str) -> io::Result<String> {
        let _ = (method, value);
        Err(io::Error::new(io::ErrorKind::Unsupported, "this compositor has no command interface"))
    }
    /// Blocks, calling `on_state` whenever the compositor's window switcher changes (LunaDash).
    fn watch_switcher(&self, on_state: &mut dyn FnMut(&SwitcherState)) -> io::Result<()> {
        let _ = on_state;
        Err(io::Error::new(io::ErrorKind::Unsupported, "this compositor has no window switcher"))
    }
}

/// The error for an action this compositor cannot perform.
fn unsupported(action: &Action) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, format!("{action:?} is not supported here"))
}

/// The IPC backend for `compositor`, if it has one and its socket is advertised.
pub fn backend(compositor: Compositor) -> Option<Arc<dyn Backend>> {
    match compositor {
        Compositor::Niri => niri::Niri::from_env().map(|b| Arc::new(b) as Arc<dyn Backend>),
        Compositor::Hyprland => {
            hyprland::Hyprland::from_env().map(|b| Arc::new(b) as Arc<dyn Backend>)
        }
        Compositor::Sway => sway::Sway::from_env().map(|b| Arc::new(b) as Arc<dyn Backend>),
        Compositor::LunaDash => {
            lunadash::LunaDash::from_env().map(|b| Arc::new(b) as Arc<dyn Backend>)
        }
        _ => None,
    }
}
