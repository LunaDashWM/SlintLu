//! Installing the platform and the backend-neutral handle to the windows it creates.

use std::rc::Rc;

use slint::platform::PlatformError;

use crate::core::{WaylandPlatform, context};
use crate::render::RendererKind;
use crate::shell::config::SurfaceRole;
use crate::window::WaylandWindow;

/// Options for [`init`].
#[derive(Clone, Debug, Default)]
pub struct PlatformConfig {
    /// The renderer new windows are created with.
    pub renderer: RendererKind,
}

/// Creates the platform and installs it as the Slint platform for this process.
pub fn init(config: PlatformConfig) -> Result<(), PlatformError> {
    let platform = WaylandPlatform::new(config)?;
    slint::platform::set_platform(Box::new(platform))
        .map_err(|e| PlatformError::from(format!("cannot install the Slint platform: {e}")))
}

/// The display-server side of a Slint window created by SlintLu, which allows changing its
/// [`SurfaceRole`] before it is shown. Get it with [`window_for`].
#[derive(Clone)]
#[non_exhaustive]
pub enum ShellWindow {
    Wayland(Rc<WaylandWindow>),
}

impl ShellWindow {
    /// The Slint window this adapter drives.
    pub fn slint_window(&self) -> &slint::Window {
        match self {
            Self::Wayland(w) => w.slint_window(),
        }
    }

    /// The renderer this window ended up with after fallbacks.
    pub fn renderer_kind(&self) -> RendererKind {
        match self {
            Self::Wayland(w) => w.renderer_kind(),
        }
    }

    /// Whether the window is currently shown.
    pub fn is_shown(&self) -> bool {
        match self {
            Self::Wayland(w) => w.is_shown(),
        }
    }

    /// The current role.
    pub fn role(&self) -> SurfaceRole {
        match self {
            Self::Wayland(w) => w.role(),
        }
    }

    /// Changes the role used when the window is shown. Layer settings can be changed at any
    /// time, except for the output and namespace, which are fixed by the first `show()`.
    pub fn set_role(&self, role: SurfaceRole) {
        match self {
            Self::Wayland(w) => w.set_role(role),
        }
    }
}

/// Returns the display-server side of a Slint window created by this platform.
pub fn window_for(window: &slint::Window) -> Option<ShellWindow> {
    let ctx = context::current()?;
    ctx.live_windows()
        .into_iter()
        .find(|w| std::ptr::eq(w.slint_window(), window))
        .map(ShellWindow::Wayland)
}
