//! [`X11Window`]: the Slint window adapter backed by one X window.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use slint::platform::{PlatformError, Renderer, WindowAdapter, WindowEvent, WindowProperties};
use slint::{LogicalSize, PhysicalSize};
use x11rb::connection::Connection;
use x11rb::properties::{WmHints, WmSizeHints};
use x11rb::protocol::xproto::{
    self, AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, Gravity,
    PropMode, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

use super::context::Context;
use super::render::SoftwareSurface;
use crate::render::RendererKind;
use crate::shell::config::SurfaceRole;
use crate::window::FrameState;
use crate::window::scale::physical;

/// Shortest time between two frames of one window (about 60 Hz): X11 has no frame callbacks.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// The X11 side of a Slint window.
///
/// Usually reached through [`crate::ShellWindow`] (see [`crate::window_for`]).
pub struct X11Window {
    surface: SoftwareSurface,
    window: slint::Window,
    ctx: Rc<Context>,
    xid: xproto::Window,
    role: RefCell<SurfaceRole>,
    title: RefCell<String>,
    frame: FrameState,
    /// Frames are not drawn before this time.
    next_frame: Cell<Option<Instant>>,
}

impl X11Window {
    pub(crate) fn new(ctx: Rc<Context>) -> Result<Rc<Self>, PlatformError> {
        let conn = &ctx.conn;
        let xid = conn.generate_id().map_err(|e| format!("cannot allocate a window id: {e}"))?;
        let events = EventMask::EXPOSURE
            | EventMask::STRUCTURE_NOTIFY
            | EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::ENTER_WINDOW
            | EventMask::LEAVE_WINDOW
            | EventMask::FOCUS_CHANGE;
        // No background: the server would clear exposed areas before the next upload.
        let aux = CreateWindowAux::new()
            .border_pixel(0)
            .colormap(ctx.visual.colormap)
            .bit_gravity(Gravity::NORTH_WEST)
            .event_mask(events);
        conn.create_window(
            ctx.visual.depth,
            xid,
            ctx.root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            ctx.visual.id,
            &aux,
        )
        .map_err(|e| format!("cannot create an X window: {e}"))?;

        let atoms = &ctx.atoms;
        let set_static_properties = || -> Result<(), x11rb::errors::ConnectionError> {
            conn.change_property32(
                PropMode::REPLACE,
                xid,
                atoms.WM_PROTOCOLS,
                AtomEnum::ATOM,
                &[atoms.WM_DELETE_WINDOW, atoms._NET_WM_PING],
            )?;
            conn.change_property32(
                PropMode::REPLACE,
                xid,
                atoms._NET_WM_PID,
                AtomEnum::CARDINAL,
                &[std::process::id()],
            )?;
            Ok(())
        };
        set_static_properties().map_err(|e| format!("cannot set window properties: {e}"))?;

        let surface = SoftwareSurface::new(&ctx, xid)?;
        let this = Rc::new_cyclic(|weak: &Weak<Self>| {
            let adapter: Weak<dyn WindowAdapter> = weak.clone();
            X11Window {
                surface,
                window: slint::Window::new(adapter),
                ctx: ctx.clone(),
                xid,
                role: RefCell::default(),
                title: RefCell::default(),
                frame: FrameState { scale: Cell::new(1.0), ..Default::default() },
                next_frame: Cell::new(None),
            }
        });
        ctx.windows.borrow_mut().push(Rc::downgrade(&this));
        Ok(this)
    }

    pub(crate) fn xid(&self) -> xproto::Window {
        self.xid
    }

    /// The Slint window this adapter drives.
    pub fn slint_window(&self) -> &slint::Window {
        &self.window
    }

    /// The renderer this window uses. Only the software renderer is available on X11.
    pub fn renderer_kind(&self) -> RendererKind {
        RendererKind::Software
    }

    /// Whether the window is currently shown.
    pub fn is_shown(&self) -> bool {
        self.frame.shown.get() && self.frame.configured.get()
    }

    /// The current role.
    pub fn role(&self) -> SurfaceRole {
        self.role.borrow().clone()
    }

    /// Changes the role used when the window is shown next.
    pub fn set_role(&self, role: SurfaceRole) {
        *self.role.borrow_mut() = role;
    }

    pub(crate) fn dispatch(&self, event: WindowEvent) {
        if let Err(e) = self.window.dispatch_event_with_result(event) {
            log::error!("error while dispatching a window event: {e}");
        }
    }

    pub(crate) fn scale(&self) -> f32 {
        self.frame.scale.get()
    }

    fn physical_size(&self) -> (u32, u32) {
        let (w, h) = self.frame.logical_size.get();
        let scale = self.frame.scale.get();
        (physical(w, scale), physical(h, scale))
    }

    fn map(&self) {
        if self.frame.shown.replace(true) {
            return;
        }
        let class = match &*self.role.borrow() {
            SurfaceRole::Toplevel { app_id } => app_id.clone(),
            SurfaceRole::Layer(config) => {
                log::warn!("layer surfaces are shown as normal windows on X11");
                config.namespace.clone()
            }
            SurfaceRole::Lock { .. } => {
                log::error!("lock screens are not supported on X11");
                self.frame.shown.set(false);
                return;
            }
        };
        if let Err(e) = self.set_toplevel_properties(&class) {
            log::error!("cannot set window properties: {e}");
        }

        let (pref_w, pref_h) = self.frame.preferred_size.get();
        let size = (pref_w.max(1), pref_h.max(1));
        if self.frame.logical_size.replace(size) != size {
            self.dispatch(WindowEvent::Resized {
                size: LogicalSize::new(size.0 as f32, size.1 as f32),
            });
        }
        let (width, height) = self.physical_size();
        let conn = &self.ctx.conn;
        let _ = conn.configure_window(self.xid, &ConfigureWindowAux::new().width(width).height(height));
        let _ = conn.map_window(self.xid);
    }

    fn set_toplevel_properties(&self, class: &str) -> Result<(), x11rb::errors::ConnectionError> {
        let conn = &self.ctx.conn;
        let atoms = &self.ctx.atoms;
        let wm_class = format!("{class}\0{class}\0");
        conn.change_property8(
            PropMode::REPLACE,
            self.xid,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            wm_class.as_bytes(),
        )?;
        conn.change_property32(
            PropMode::REPLACE,
            self.xid,
            atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[atoms._NET_WM_WINDOW_TYPE_NORMAL],
        )?;
        WmHints { input: Some(true), ..WmHints::new() }.set(conn, self.xid)?;
        self.set_title(&self.title.borrow())?;
        self.set_min_size(self.frame.min_size.get())?;
        Ok(())
    }

    fn set_title(&self, title: &str) -> Result<(), x11rb::errors::ConnectionError> {
        let conn = &self.ctx.conn;
        let utf8 = self.ctx.atoms.UTF8_STRING;
        for property in [self.ctx.atoms._NET_WM_NAME, AtomEnum::WM_NAME.into()] {
            conn.change_property8(PropMode::REPLACE, self.xid, property, utf8, title.as_bytes())?;
        }
        Ok(())
    }

    fn set_min_size(&self, min: Option<(u32, u32)>) -> Result<(), x11rb::errors::ConnectionError> {
        let scale = self.frame.scale.get();
        let hints = WmSizeHints {
            min_size: min.map(|(w, h)| (physical(w, scale) as i32, physical(h, scale) as i32)),
            ..WmSizeHints::new()
        };
        hints.set_normal_hints(&self.ctx.conn, self.xid)?;
        Ok(())
    }

    fn unmap(&self) {
        if !self.frame.shown.replace(false) {
            return;
        }
        self.frame.configured.set(false);
        let _ = self.ctx.conn.unmap_window(self.xid);
    }

    /// The server mapped the window: it can be drawn now.
    pub(crate) fn mapped(&self) {
        if self.frame.shown.get() {
            self.frame.configured.set(true);
            self.frame.dirty.set(true);
        }
    }

    /// The window was resized, by us or by the window manager.
    pub(crate) fn resized(&self, width: u16, height: u16) {
        let scale = self.frame.scale.get();
        let logical = |px: u16| ((px as f32 / scale).round() as u32).max(1);
        let size = (logical(width), logical(height));
        if self.frame.logical_size.replace(size) != size {
            self.dispatch(WindowEvent::Resized {
                size: LogicalSize::new(size.0 as f32, size.1 as f32),
            });
            self.frame.dirty.set(true);
        }
    }

    /// Part of the window must be drawn again: upload the last frame, or render one.
    pub(crate) fn exposed(&self) {
        if !self.is_shown() {
            return;
        }
        let (width, height) = self.physical_size();
        match self.surface.redraw(&self.ctx, self.xid, width, height) {
            Ok(true) => {}
            Ok(false) => self.frame.dirty.set(true),
            Err(e) => log::error!("{e}"),
        }
    }

    pub(crate) fn request_close(&self) {
        self.dispatch(WindowEvent::CloseRequested);
    }

    /// Draws a frame if one is needed. Returns when the next one is due if the window is still
    /// dirty afterwards (animations, or a redraw that came too soon after the last frame).
    pub(crate) fn render_if_needed(&self, now: Instant) -> Option<Instant> {
        let frame = &self.frame;
        if !frame.shown.get() || !frame.configured.get() || !frame.dirty.get() {
            return None;
        }
        if let Some(next) = self.next_frame.get()
            && now < next
        {
            return Some(next);
        }
        frame.dirty.set(false);
        let (width, height) = self.physical_size();
        if width == 0 || height == 0 {
            return None;
        }
        if let Err(e) = self.surface.render(&self.ctx, self.xid, width, height) {
            log::error!("rendering failed: {e}");
        }
        let next = now + FRAME_INTERVAL;
        self.next_frame.set(Some(next));
        if self.window.has_active_animations() {
            frame.dirty.set(true);
            return Some(next);
        }
        None
    }

    fn properties_changed(&self, title: String, preferred: (u32, u32), min: Option<(u32, u32)>) {
        if *self.title.borrow() != title {
            if let Err(e) = self.set_title(&title) {
                log::warn!("cannot set the window title: {e}");
            }
            *self.title.borrow_mut() = title;
        }
        self.frame.preferred_size.set(preferred);
        if self.frame.min_size.replace(min) != min
            && let Err(e) = self.set_min_size(min)
        {
            log::warn!("cannot set the minimum window size: {e}");
        }
    }
}

impl Drop for X11Window {
    fn drop(&mut self) {
        self.surface.destroy(&self.ctx);
        let _ = self.ctx.conn.destroy_window(self.xid);
        let _ = self.ctx.conn.flush();
    }
}

impl WindowAdapter for X11Window {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if visible {
            self.map()
        } else {
            self.unmap()
        }
        Ok(())
    }

    fn size(&self) -> PhysicalSize {
        let (w, h) = self.physical_size();
        PhysicalSize::new(w, h)
    }

    fn renderer(&self) -> &dyn Renderer {
        &self.surface.renderer
    }

    fn request_redraw(&self) {
        self.frame.dirty.set(true);
    }

    fn update_window_properties(&self, properties: WindowProperties<'_>) {
        let title = properties.title().to_string();
        let constraints = properties.layout_constraints();
        let to_px = |s: slint::LogicalSize| (s.width.ceil() as u32, s.height.ceil() as u32);
        self.properties_changed(title, to_px(constraints.preferred), constraints.min.map(to_px));
    }
}
