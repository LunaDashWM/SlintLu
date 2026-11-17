//! OpenGL ES through EGL on Wayland (and X11 through `EGL_EXT_platform_xcb`), for Slint's
//! FemtoVG renderer.
//!
//! `libEGL.so.1` is loaded at runtime, so the binary still starts (with the software renderer)
//! on machines without any GL driver.

use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::num::NonZeroU32;
use std::rc::Rc;

use khronos_egl as egl;
use slint::platform::femtovg_renderer::OpenGLInterface;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use wayland_egl::WlEglSurface;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Egl = egl::DynamicInstance<egl::EGL1_4>;

/// `EGL_PLATFORM_WAYLAND_KHR`
const PLATFORM_WAYLAND: egl::Enum = 0x31D8;
/// `EGL_PLATFORM_XCB_EXT`
#[cfg(feature = "x11")]
const PLATFORM_XCB: egl::Enum = 0x31DC;
/// `EGL_PLATFORM_XCB_SCREEN_EXT`
#[cfg(feature = "x11")]
const PLATFORM_XCB_SCREEN: egl::Attrib = 0x31DE;

/// The shared EGL display of a platform: `None` until first use, `Some(None)` if
/// initialisation failed.
pub(crate) type SharedEgl = RefCell<Option<Option<Rc<EglDisplay>>>>;

/// The connection EGL renders through. It must outlive the [`EglDisplay`].
#[derive(Clone, Copy)]
pub(crate) enum NativeDisplay {
    /// A `wl_display*`.
    Wayland(*mut c_void),
    /// An `xcb_connection_t*`, its screen and the visual windows are created with.
    #[cfg(feature = "x11")]
    Xcb { connection: *mut c_void, screen: i32, visual: u32 },
}

/// The process-wide EGL display, shared by all windows.
pub(crate) struct EglDisplay {
    egl: Egl,
    display: egl::Display,
    config: egl::Config,
}

const CONFIG_ATTRIBUTES: [egl::Int; 15] = [
    egl::SURFACE_TYPE,
    egl::WINDOW_BIT,
    egl::RENDERABLE_TYPE,
    egl::OPENGL_ES2_BIT,
    egl::RED_SIZE,
    8,
    egl::GREEN_SIZE,
    8,
    egl::BLUE_SIZE,
    8,
    egl::ALPHA_SIZE,
    8,
    // FemtoVG fills paths through the stencil buffer.
    egl::STENCIL_SIZE,
    8,
    egl::NONE,
];

impl EglDisplay {
    /// Initialises EGL once per platform; later calls return the cached result.
    pub fn shared(cache: &SharedEgl, native: NativeDisplay) -> Option<Rc<Self>> {
        cache
            .borrow_mut()
            .get_or_insert_with(|| {
                Self::new(native)
                    .inspect_err(|e| log::warn!("cannot initialise EGL: {e}"))
                    .ok()
                    .map(Rc::new)
            })
            .clone()
    }

    fn new(native: NativeDisplay) -> Result<Self, String> {
        // SAFETY: loading the system EGL library; its initialisers have no preconditions.
        let egl =
            unsafe { Egl::load_required() }.map_err(|e| format!("cannot load libEGL: {e}"))?;

        let display = match native {
            // Prefer the EGL 1.5 platform API; fall back to the legacy entry point which Mesa
            // and NVIDIA also accept for a `wl_display*`.
            NativeDisplay::Wayland(wl_display) => match egl.upcast::<egl::EGL1_5>() {
                Some(egl15) => {
                    // SAFETY: `wl_display` is the live `wl_display*` of our connection.
                    unsafe {
                        egl15.get_platform_display(
                            PLATFORM_WAYLAND,
                            wl_display,
                            &[egl::ATTRIB_NONE],
                        )
                    }
                    .map_err(|e| e.to_string())?
                }
                // SAFETY: as above.
                None => unsafe { egl.get_display(wl_display) }.ok_or("no EGL display for wayland")?,
            },
            #[cfg(feature = "x11")]
            NativeDisplay::Xcb { connection, screen, .. } => {
                let egl15 = egl.upcast::<egl::EGL1_5>().ok_or("EGL 1.5 is needed on X11")?;
                // SAFETY: `connection` is the live `xcb_connection_t*` of the X11 platform.
                unsafe {
                    egl15.get_platform_display(
                        PLATFORM_XCB,
                        connection,
                        &[PLATFORM_XCB_SCREEN, screen as egl::Attrib, egl::ATTRIB_NONE],
                    )
                }
                .map_err(|e| format!("no EGL display for X11 (EGL_EXT_platform_xcb): {e}"))?
            }
        };
        egl.initialize(display).map_err(|e| format!("eglInitialize: {e}"))?;
        egl.bind_api(egl::OPENGL_ES_API).map_err(|e| format!("eglBindAPI: {e}"))?;

        let config = match native {
            NativeDisplay::Wayland(_) => egl
                .choose_first_config(display, &CONFIG_ATTRIBUTES)
                .map_err(|e| format!("eglChooseConfig: {e}"))?
                .ok_or("no RGBA8 + stencil EGL config")?,
            // X windows only accept configs of their own visual.
            #[cfg(feature = "x11")]
            NativeDisplay::Xcb { visual, .. } => {
                let count = egl
                    .matching_config_count(display, &CONFIG_ATTRIBUTES)
                    .map_err(|e| format!("eglChooseConfig: {e}"))?;
                let mut configs = Vec::with_capacity(count);
                egl.choose_config(display, &CONFIG_ATTRIBUTES, &mut configs)
                    .map_err(|e| format!("eglChooseConfig: {e}"))?;
                configs
                    .into_iter()
                    .find(|&c| {
                        egl.get_config_attrib(display, c, egl::NATIVE_VISUAL_ID)
                            .is_ok_and(|id| id as u32 == visual)
                    })
                    .ok_or("no RGBA8 + stencil EGL config for the window visual")?
            }
        };
        Ok(Self { egl, display, config })
    }

    fn create_context(&self) -> Result<egl::Context, String> {
        [3, 2]
            .into_iter()
            .find_map(|version| {
                self.egl
                    .create_context(
                        self.display,
                        self.config,
                        None,
                        &[egl::CONTEXT_CLIENT_VERSION, version, egl::NONE],
                    )
                    .ok()
            })
            .ok_or_else(|| "cannot create an OpenGL ES 2 or 3 context".into())
    }
}

/// Loads EGL and initialises a display on the running platform's connection (for
/// diagnostics).
pub(crate) fn probe() -> Result<String, String> {
    let native = if let Some(ctx) = crate::core::context::current() {
        NativeDisplay::Wayland(ctx.conn.backend().display_ptr() as *mut c_void)
    } else {
        #[cfg(feature = "x11")]
        let native = crate::x11::context::current().map(|ctx| ctx.egl_display());
        #[cfg(not(feature = "x11"))]
        let native = None;
        native.ok_or("the platform is not running")?
    };
    let display = EglDisplay::new(native)?;
    let vendor = display
        .egl
        .query_string(Some(display.display), egl::VENDOR)
        .map(|s| s.to_string_lossy().into_owned());
    let version = display
        .egl
        .query_string(Some(display.display), egl::VERSION)
        .map(|s| s.to_string_lossy().into_owned());
    Ok(format!("EGL {} ({})", version.unwrap_or_default(), vendor.unwrap_or_default()))
}

/// What the EGL surface renders into.
enum NativeSurface {
    Wayland(WlEglSurface),
    /// X windows are resized by the server; nothing to keep.
    #[cfg(feature = "x11")]
    Xcb,
}

/// A GL context bound to one window's surface.
pub(crate) struct EglWindow {
    display: Rc<EglDisplay>,
    context: egl::Context,
    surface: egl::Surface,
    // Dropped after the EGL surface that renders into it.
    native: NativeSurface,
}

impl EglWindow {
    pub fn wayland(display: Rc<EglDisplay>, wl_surface: &WlSurface) -> Result<Self, String> {
        let native = WlEglSurface::new(wl_surface.id(), 1, 1).map_err(|e| e.to_string())?;
        // SAFETY: `native` is a valid wl_egl_window that outlives the EGL surface (field order).
        let surface = unsafe {
            display.egl.create_window_surface(
                display.display,
                display.config,
                native.ptr() as egl::NativeWindowType,
                None,
            )
        }
        .map_err(|e| format!("eglCreateWindowSurface: {e}"))?;
        Self::finish(display, surface, NativeSurface::Wayland(native))
    }

    /// A surface for the X window `window`, which must outlive it.
    #[cfg(feature = "x11")]
    pub fn xcb(display: Rc<EglDisplay>, window: u32) -> Result<Self, String> {
        let egl15 = display.egl.upcast::<egl::EGL1_5>().ok_or("EGL 1.5 is needed on X11")?;
        let mut window = window;
        // SAFETY: EGL_EXT_platform_xcb takes a pointer to the `xcb_window_t`, read during the
        // call; the window itself outlives the surface (field order in `X11Window`).
        let surface = unsafe {
            egl15.create_platform_window_surface(
                display.display,
                display.config,
                (&raw mut window).cast(),
                &[egl::ATTRIB_NONE],
            )
        }
        .map_err(|e| format!("eglCreatePlatformWindowSurface: {e}"))?;
        Self::finish(display, surface, NativeSurface::Xcb)
    }

    fn finish(
        display: Rc<EglDisplay>,
        surface: egl::Surface,
        native: NativeSurface,
    ) -> Result<Self, String> {
        let context = match display.create_context() {
            Ok(context) => context,
            Err(e) => {
                let _ = display.egl.destroy_surface(display.display, surface);
                return Err(e);
            }
        };
        let this = Self { display, context, surface, native };
        this.ensure_current().map_err(|e| e.to_string())?;
        // Frames are paced by the platform (frame callbacks on Wayland, a timer on X11); never
        // block in eglSwapBuffers.
        let _ = this.display.egl.swap_interval(this.display.display, 0);
        Ok(this)
    }
}

// SAFETY: every function forwards to EGL, and `get_proc_address` returns EGL's own pointers.
unsafe impl OpenGLInterface for EglWindow {
    fn ensure_current(&self) -> Result<(), Error> {
        let d = &self.display;
        d.egl.make_current(
            d.display,
            Some(self.surface),
            Some(self.surface),
            Some(self.context),
        )?;
        Ok(())
    }

    fn swap_buffers(&self) -> Result<(), Error> {
        self.display.egl.swap_buffers(self.display.display, self.surface)?;
        Ok(())
    }

    fn resize(&self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Error> {
        match &self.native {
            NativeSurface::Wayland(native) => {
                native.resize(width.get() as i32, height.get() as i32, 0, 0)
            }
            #[cfg(feature = "x11")]
            NativeSurface::Xcb => {}
        }
        Ok(())
    }

    fn get_proc_address(&self, name: &CStr) -> *const c_void {
        name.to_str()
            .ok()
            .and_then(|name| self.display.egl.get_proc_address(name))
            .map_or(std::ptr::null(), |f| f as *const c_void)
    }
}

impl Drop for EglWindow {
    fn drop(&mut self) {
        let d = &self.display;
        let _ = d.egl.make_current(d.display, None, None, None);
        let _ = d.egl.destroy_surface(d.display, self.surface);
        let _ = d.egl.destroy_context(d.display, self.context);
    }
}
