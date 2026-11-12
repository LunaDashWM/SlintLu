//! Wayland objects and bookkeeping shared by the platform, the dispatcher and the windows.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::session_lock::{SessionLock, SessionLockState};
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shm::Shm;

use super::state::State;
use crate::output::OutputInfo;
use crate::render::RendererKind;
use crate::window::WaylandWindow;

/// A callback for session lock events.
pub(crate) type LockListener = Rc<dyn Fn(crate::security::LockEvent)>;

/// Only ever borrowed briefly and never across calls into Slint, so that Slint callbacks may
/// freely create or destroy windows while an event is being dispatched.
pub(crate) struct Context {
    pub conn: Connection,
    pub qh: QueueHandle<State>,
    pub compositor: CompositorState,
    pub layer_shell: Option<LayerShell>,
    pub xdg_shell: Option<XdgShell>,
    pub shm: Shm,
    pub clipboard: crate::clipboard::ClipboardState,
    pub clipboard_history: crate::clipboard::ClipboardHistory,
    pub fractional_scaling: Option<crate::window::scale::FractionalScaling>,
    pub idle: crate::idle::IdleState,
    pub renderer: RendererKind,
    /// Renderers that already failed for some window: later failures are logged at debug level.
    pub failed_renderers: RefCell<Vec<RendererKind>>,
    /// Interface names of every global the compositor advertised at startup.
    pub globals: Vec<String>,
    pub windows: RefCell<Vec<Weak<WaylandWindow>>>,
    pub outputs: RefCell<Vec<(OutputInfo, WlOutput)>>,
    pub output_listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    pub session_lock_state: Option<SessionLockState>,
    /// The active (or requested) session lock.
    pub session_lock: RefCell<Option<SessionLock>>,
    pub lock_listeners: RefCell<Vec<LockListener>>,
    /// Shared EGL display: `None` until first use, `Some(None)` if initialisation failed.
    #[cfg(feature = "opengl")]
    pub egl: crate::render::egl::SharedEgl,
    /// Shared wgpu device, with the same states as `egl`.
    #[cfg(feature = "vulkan")]
    pub gpu: crate::render::gpu::SharedGpu,
}

impl Context {
    pub fn find_window(&self, surface: &WlSurface) -> Option<Rc<WaylandWindow>> {
        self.windows.borrow().iter().filter_map(Weak::upgrade).find(|w| w.wl_surface() == surface)
    }

    pub fn live_windows(&self) -> Vec<Rc<WaylandWindow>> {
        let mut windows = self.windows.borrow_mut();
        windows.retain(|w| w.strong_count() > 0);
        windows.iter().filter_map(Weak::upgrade).collect()
    }

    /// Drops the shared graphics contexts. Windows keep their own references; new windows
    /// initialise them again. Must happen before thread-local destructors run, because wgpu
    /// and EGL teardown use thread-locals themselves.
    pub fn release_graphics(&self) {
        #[cfg(feature = "opengl")]
        self.egl.borrow_mut().take();
        #[cfg(feature = "vulkan")]
        self.gpu.borrow_mut().take();
    }

    /// Draws every window that asked for a redraw and is ready for a new frame.
    pub fn render_dirty_windows(&self) {
        for window in self.live_windows() {
            window.render_if_needed();
        }
    }
}

thread_local! {
    static CONTEXT: RefCell<Weak<Context>> = const { RefCell::new(Weak::new()) };
}

pub(crate) fn install(ctx: &Rc<Context>) {
    CONTEXT.with(|c| *c.borrow_mut() = Rc::downgrade(ctx));
}

/// The context of the platform running on this thread, if any.
pub(crate) fn current() -> Option<Rc<Context>> {
    CONTEXT.with(|c| c.borrow().upgrade())
}
