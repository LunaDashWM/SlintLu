//! [`WaylandWindow`]: the Slint window adapter backed by one `wl_surface`.

mod adapter;
mod frame;
mod lifecycle;
pub(crate) mod scale;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use slint::platform::{PlatformError, WindowAdapter, WindowEvent};
use smithay_client_toolkit::compositor::Surface;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

use crate::core::context::Context;
use crate::render::{RendererKind, WindowRenderer};
use crate::shell::RoleObject;
use crate::shell::config::SurfaceRole;

/// Geometry and frame bookkeeping, all in plain cells so that no borrow is ever held while
/// Slint code runs.
#[derive(Default)]
pub(crate) struct FrameState {
    /// Size last configured by the compositor, in logical pixels.
    pub logical_size: Cell<(u32, u32)>,
    /// Preferred size from the Slint layout, in logical pixels.
    pub preferred_size: Cell<(u32, u32)>,
    pub min_size: Cell<Option<(u32, u32)>>,
    /// Scale factor; fractional when the compositor supports fractional-scale-v1.
    pub scale: Cell<f32>,
    /// `set_visible(true)` was called and not undone.
    pub shown: Cell<bool>,
    /// The compositor acknowledged the current mapping with a configure event.
    pub configured: Cell<bool>,
    pub dirty: Cell<bool>,
    /// A frame callback is outstanding; drawing now would only waste work.
    pub frame_pending: Cell<bool>,
}

/// The Wayland side of a Slint window.
///
/// Usually reached through [`crate::ShellWindow`] (see [`crate::window_for`]), which also sets
/// its [`SurfaceRole`] before `show()`.
pub struct WaylandWindow {
    // Field order matters: the renderer holds GPU surfaces that reference `wl_surface`, so it
    // must be dropped before the role object destroys the surface.
    renderer: WindowRenderer,
    window: slint::Window,
    ctx: Rc<Context>,
    wl_surface: WlSurface,
    role_object: RefCell<RoleObject>,
    role: RefCell<SurfaceRole>,
    title: RefCell<String>,
    frame: FrameState,
    /// Fractional scaling objects; `None` means integer `wl_surface` buffer scaling.
    scaling: Option<scale::SurfaceScaling>,
}

impl WaylandWindow {
    pub(crate) fn new(ctx: Rc<Context>) -> Result<Rc<Self>, PlatformError> {
        let surface = Surface::new(&ctx.compositor, &ctx.qh)
            .map_err(|e| format!("cannot create a wl_surface: {e}"))?;
        let wl_surface = surface.wl_surface().clone();
        let renderer = WindowRenderer::new(&ctx, &wl_surface)?;
        let scaling = ctx.fractional_scaling.as_ref().map(|f| f.for_surface(&wl_surface, &ctx.qh));

        let this = Rc::new_cyclic(|weak: &Weak<Self>| {
            let adapter: Weak<dyn WindowAdapter> = weak.clone();
            WaylandWindow {
                renderer,
                window: slint::Window::new(adapter),
                ctx: ctx.clone(),
                wl_surface,
                role_object: RefCell::new(RoleObject::Unassigned(Some(surface))),
                role: RefCell::default(),
                title: RefCell::default(),
                frame: FrameState { scale: Cell::new(1.0), ..Default::default() },
                scaling,
            }
        });
        ctx.windows.borrow_mut().push(Rc::downgrade(&this));
        Ok(this)
    }

    pub(crate) fn wl_surface(&self) -> &WlSurface {
        &self.wl_surface
    }

    /// The Slint window this adapter drives.
    pub fn slint_window(&self) -> &slint::Window {
        &self.window
    }

    /// The renderer this window ended up with after fallbacks.
    pub fn renderer_kind(&self) -> RendererKind {
        self.renderer.kind()
    }

    /// Whether the window is currently shown.
    pub fn is_shown(&self) -> bool {
        self.frame.shown.get() && self.frame.configured.get()
    }

    /// The current role.
    pub fn role(&self) -> SurfaceRole {
        self.role.borrow().clone()
    }

    /// Changes the role used when the window is shown. Layer settings can be changed at any
    /// time, except for the output and namespace, which are fixed by the first `show()`.
    pub fn set_role(&self, role: SurfaceRole) {
        *self.role.borrow_mut() = role;
        self.role_changed();
    }

    pub(crate) fn dispatch(&self, event: WindowEvent) {
        if let Err(e) = self.window.dispatch_event_with_result(event) {
            log::error!("error while dispatching a window event: {e}");
        }
    }
}
