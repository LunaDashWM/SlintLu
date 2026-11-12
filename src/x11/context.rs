//! X11 objects and bookkeeping shared by the platform, the event handlers and the windows.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Instant;

use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageOrder, Screen, VisualClass};
use x11rb::xcb_ffi::XCBConnection;

use super::atoms::Atoms;
use super::keymap::Keymap;
use super::layer::Rect;
use super::window::X11Window;
use crate::output::OutputInfo;
use crate::render::RendererKind;

/// The visual every window is created with: 32-bit ARGB when the server has one, so that
/// shells can be translucent under a compositing manager.
pub(crate) struct Visual {
    pub id: xproto::Visualid,
    pub depth: u8,
    pub colormap: xproto::Colormap,
}

impl Visual {
    pub fn choose(conn: &XCBConnection, screen: &Screen) -> Result<Self, String> {
        let setup = x11rb::connection::Connection::setup(conn);
        if setup.image_byte_order != ImageOrder::LSB_FIRST {
            return Err("X servers with big-endian images are not supported".into());
        }
        let is_argb = |v: &xproto::Visualtype| {
            v.class == VisualClass::TRUE_COLOR
                && (v.red_mask, v.green_mask, v.blue_mask) == (0xff0000, 0xff00, 0xff)
        };
        let argb = screen
            .allowed_depths
            .iter()
            .filter(|d| d.depth == 32)
            .flat_map(|d| d.visuals.iter())
            .find(|v| is_argb(v));
        let bits_per_pixel = |depth: u8| {
            setup.pixmap_formats.iter().find(|f| f.depth == depth).map(|f| f.bits_per_pixel)
        };

        if let Some(visual) = argb
            && bits_per_pixel(32) == Some(32)
        {
            let colormap = x11rb::connection::Connection::generate_id(conn)
                .map_err(|e| format!("cannot allocate a colormap id: {e}"))?;
            conn.create_colormap(
                xproto::ColormapAlloc::NONE,
                colormap,
                screen.root,
                visual.visual_id,
            )
            .map_err(|e| format!("cannot create a colormap: {e}"))?;
            return Ok(Self { id: visual.visual_id, depth: 32, colormap });
        }

        log::info!("the X server has no 32-bit ARGB visual: windows will be opaque");
        if bits_per_pixel(screen.root_depth) != Some(32) {
            return Err(format!(
                "unsupported root window depth {} (24 or 32 bits are needed)",
                screen.root_depth
            ));
        }
        Ok(Self { id: screen.root_visual, depth: screen.root_depth, colormap: screen.default_colormap })
    }
}

/// Only ever borrowed briefly and never across calls into Slint, so that Slint callbacks may
/// freely create or destroy windows while an event is being dispatched.
pub(crate) struct Context {
    pub conn: XCBConnection,
    /// Index of the default screen in the connection setup.
    pub screen: usize,
    pub root: xproto::Window,
    pub atoms: Atoms,
    pub visual: Visual,
    /// Whether RandR 1.5 (monitors) is available.
    pub randr: bool,
    pub keymap: RefCell<Keymap>,
    /// Scale factor from `Xft.dpi`, for every window (X11 has no per-monitor scale).
    pub scale: Cell<f32>,
    pub renderer: RendererKind,
    /// Renderers that already failed for some window: later failures are logged at debug level.
    pub failed_renderers: RefCell<Vec<RendererKind>>,
    pub windows: RefCell<Vec<Weak<X11Window>>>,
    /// Outputs with their area in physical pixels.
    pub outputs: RefCell<Vec<(OutputInfo, Rect)>>,
    pub output_listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    #[cfg(feature = "opengl")]
    pub egl: crate::render::egl::SharedEgl,
    #[cfg(feature = "vulkan")]
    pub gpu: crate::render::gpu::SharedGpu,
}

impl Context {
    pub fn find_window(&self, xid: xproto::Window) -> Option<Rc<X11Window>> {
        self.windows.borrow().iter().filter_map(Weak::upgrade).find(|w| w.xid() == xid)
    }

    pub fn live_windows(&self) -> Vec<Rc<X11Window>> {
        let mut windows = self.windows.borrow_mut();
        windows.retain(|w| w.strong_count() > 0);
        windows.iter().filter_map(Weak::upgrade).collect()
    }

    /// The area of output `id` in physical pixels; the first output, or the whole screen, when
    /// it is gone or `None`.
    pub fn output_rect(&self, id: Option<u32>) -> Rect {
        let outputs = self.outputs.borrow();
        id.and_then(|id| outputs.iter().find(|(info, _)| info.id == id))
            .or_else(|| outputs.first())
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| {
                let (width, height) = self.root_size();
                Rect { x: 0, y: 0, width, height }
            })
    }

    /// The size of the whole X screen.
    pub fn root_size(&self) -> (u32, u32) {
        let screen = &x11rb::connection::Connection::setup(&self.conn).roots[self.screen];
        (screen.width_in_pixels.into(), screen.height_in_pixels.into())
    }

    /// The EGL view of this connection.
    #[cfg(feature = "opengl")]
    pub fn egl_display(&self) -> crate::render::egl::NativeDisplay {
        crate::render::egl::NativeDisplay::Xcb {
            connection: self.conn.get_raw_xcb_connection(),
            screen: self.screen as i32,
            visual: self.visual.id,
        }
    }

    /// Drops the shared graphics contexts; see the Wayland context's `release_graphics`.
    pub fn release_graphics(&self) {
        #[cfg(feature = "opengl")]
        self.egl.borrow_mut().take();
        #[cfg(feature = "vulkan")]
        self.gpu.borrow_mut().take();
    }

    /// Draws every window that asked for a redraw. Returns when the next frame of a window
    /// that is still dirty (animating, or throttled) is due.
    pub fn render_dirty_windows(&self) -> Option<Instant> {
        let now = Instant::now();
        self.live_windows().iter().filter_map(|w| w.render_if_needed(now)).min()
    }
}

thread_local! {
    static CONTEXT: RefCell<Weak<Context>> = const { RefCell::new(Weak::new()) };
}

pub(crate) fn install(ctx: &Rc<Context>) {
    CONTEXT.with(|c| *c.borrow_mut() = Rc::downgrade(ctx));
}

/// The context of the X11 platform running on this thread, if any.
pub(crate) fn current() -> Option<Rc<Context>> {
    CONTEXT.with(|c| c.borrow().upgrade())
}
