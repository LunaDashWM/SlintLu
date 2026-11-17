//! Choosing and installing the platform, and the backend-neutral handle to the windows it
//! creates.

use std::rc::Rc;
use std::str::FromStr;

use slint::platform::PlatformError;

use crate::core::{WaylandPlatform, context};
use crate::render::RendererKind;
use crate::shell::config::SurfaceRole;
use crate::window::WaylandWindow;
#[cfg(feature = "x11")]
use crate::x11::{X11Platform, X11Window};

/// The display server SlintLu talks to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    /// Wayland when a compositor is running (`$WAYLAND_DISPLAY`), X11 otherwise.
    #[default]
    Auto,
    Wayland,
    /// X11 / Xorg through `$DISPLAY`. Needs the `x11` feature.
    X11,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::Wayland => "wayland",
            Backend::X11 => "x11",
        }
    }
}

impl FromStr for Backend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" | "" => Ok(Self::Auto),
            "wayland" | "wl" => Ok(Self::Wayland),
            "x11" | "x" | "xorg" => Ok(Self::X11),
            other => Err(format!("unknown backend `{other}` (expected auto, wayland or x11)")),
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Options for [`init`].
#[derive(Clone, Debug, Default)]
pub struct PlatformConfig {
    /// The renderer new windows are created with.
    pub renderer: RendererKind,
    /// The display server to connect to.
    pub backend: Backend,
}

fn install(platform: impl slint::platform::Platform + 'static) -> Result<(), PlatformError> {
    slint::platform::set_platform(Box::new(platform))
        .map_err(|e| PlatformError::from(format!("cannot install the Slint platform: {e}")))
}

fn has_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

#[cfg(feature = "x11")]
fn init_x11(config: PlatformConfig) -> Result<(), PlatformError> {
    install(X11Platform::new(config)?)
}

#[cfg(not(feature = "x11"))]
fn init_x11(_: PlatformConfig) -> Result<(), PlatformError> {
    Err(PlatformError::from("X11 support was not compiled in (the `x11` feature)"))
}

/// Creates the platform for the configured [`Backend`] and installs it as the Slint platform
/// for this process.
pub fn init(config: PlatformConfig) -> Result<(), PlatformError> {
    match config.backend {
        Backend::Wayland => install(WaylandPlatform::new(config)?),
        Backend::X11 => init_x11(config),
        Backend::Auto => {
            let wayland = has_env("WAYLAND_DISPLAY") || has_env("WAYLAND_SOCKET");
            let x11 = cfg!(feature = "x11") && has_env("DISPLAY");
            if !wayland && x11 {
                return init_x11(config);
            }
            match WaylandPlatform::new(config.clone()) {
                Ok(platform) => install(platform),
                Err(e) if x11 => {
                    log::warn!("{e}; trying X11");
                    init_x11(config)
                }
                Err(e) => Err(e),
            }
        }
    }
}

/// The backend of the platform running on this thread, if [`init`] succeeded.
pub fn active_backend() -> Option<Backend> {
    if context::current().is_some() {
        return Some(Backend::Wayland);
    }
    #[cfg(feature = "x11")]
    if crate::x11::context::current().is_some() {
        return Some(Backend::X11);
    }
    None
}

/// The display-server side of a Slint window created by SlintLu, which allows changing its
/// [`SurfaceRole`] before it is shown. Get it with [`window_for`].
#[derive(Clone)]
#[non_exhaustive]
pub enum ShellWindow {
    Wayland(Rc<WaylandWindow>),
    #[cfg(feature = "x11")]
    X11(Rc<X11Window>),
}

impl ShellWindow {
    /// The Slint window this adapter drives.
    pub fn slint_window(&self) -> &slint::Window {
        match self {
            Self::Wayland(w) => w.slint_window(),
            #[cfg(feature = "x11")]
            Self::X11(w) => w.slint_window(),
        }
    }

    /// The renderer this window ended up with after fallbacks.
    pub fn renderer_kind(&self) -> RendererKind {
        match self {
            Self::Wayland(w) => w.renderer_kind(),
            #[cfg(feature = "x11")]
            Self::X11(w) => w.renderer_kind(),
        }
    }

    /// Whether the window is currently shown.
    pub fn is_shown(&self) -> bool {
        match self {
            Self::Wayland(w) => w.is_shown(),
            #[cfg(feature = "x11")]
            Self::X11(w) => w.is_shown(),
        }
    }

    /// The current role.
    pub fn role(&self) -> SurfaceRole {
        match self {
            Self::Wayland(w) => w.role(),
            #[cfg(feature = "x11")]
            Self::X11(w) => w.role(),
        }
    }

    /// Changes the role used when the window is shown. Layer settings can be changed at any
    /// time, except for the output and namespace, which are fixed by the first `show()`.
    pub fn set_role(&self, role: SurfaceRole) {
        match self {
            Self::Wayland(w) => w.set_role(role),
            #[cfg(feature = "x11")]
            Self::X11(w) => w.set_role(role),
        }
    }
}

/// Returns the display-server side of a Slint window created by this platform.
pub fn window_for(window: &slint::Window) -> Option<ShellWindow> {
    if let Some(ctx) = context::current() {
        return ctx
            .live_windows()
            .into_iter()
            .find(|w| std::ptr::eq(w.slint_window(), window))
            .map(ShellWindow::Wayland);
    }
    #[cfg(feature = "x11")]
    if let Some(ctx) = crate::x11::context::current() {
        return ctx
            .live_windows()
            .into_iter()
            .find(|w| std::ptr::eq(w.slint_window(), window))
            .map(ShellWindow::X11);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_parses_aliases() {
        assert_eq!("X11".parse::<Backend>().unwrap(), Backend::X11);
        assert_eq!("xorg".parse::<Backend>().unwrap(), Backend::X11);
        assert_eq!("wayland".parse::<Backend>().unwrap(), Backend::Wayland);
        assert_eq!("".parse::<Backend>().unwrap(), Backend::Auto);
        assert!("mir".parse::<Backend>().is_err());
    }
}
