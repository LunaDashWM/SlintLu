//! Renderers. Each window picks the first renderer from [`RendererKind::candidates`] that can
//! be initialised, so a missing GPU driver never prevents the shell from starting.

#[cfg(feature = "opengl")]
pub(crate) mod egl;
#[cfg(feature = "vulkan")]
pub(crate) mod gpu;
mod kind;
pub(crate) mod pixel;
mod software;

use slint::platform::PlatformError;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

pub use kind::RendererKind;

use crate::core::context::Context;

pub(crate) enum WindowRenderer {
    Software(Box<software::SoftwareBackend>),
    #[cfg(feature = "opengl")]
    OpenGL(Box<slint::platform::femtovg_renderer::FemtoVGRenderer>),
    #[cfg(feature = "vulkan")]
    Vulkan(Box<gpu::GpuWindow>),
}

impl WindowRenderer {
    pub fn new(ctx: &Context, surface: &WlSurface) -> Result<Self, PlatformError> {
        let mut last_error = String::new();
        for &kind in ctx.renderer.candidates() {
            match Self::try_new(kind, ctx, surface) {
                Ok(renderer) => {
                    log::debug!("window uses the {kind} renderer");
                    return Ok(renderer);
                }
                Err(e) => {
                    // Warn once per renderer; every window tries the same ones.
                    let mut failed = ctx.failed_renderers.borrow_mut();
                    if failed.contains(&kind) {
                        log::debug!("{kind} renderer unavailable: {e}");
                    } else {
                        log::warn!("{kind} renderer unavailable: {e}");
                        failed.push(kind);
                    }
                    last_error = e;
                }
            }
        }
        Err(PlatformError::from(format!("no usable renderer: {last_error}")))
    }

    #[cfg_attr(not(any(feature = "opengl", feature = "vulkan")), allow(unused_variables))]
    fn try_new(kind: RendererKind, ctx: &Context, surface: &WlSurface) -> Result<Self, String> {
        match kind {
            RendererKind::Software => {
                Ok(Self::Software(Box::new(software::SoftwareBackend::new())))
            }
            #[cfg(feature = "opengl")]
            RendererKind::OpenGL => {
                let native = ctx.conn.backend().display_ptr() as *mut std::ffi::c_void;
                let display = egl::EglDisplay::shared(&ctx.egl, egl::NativeDisplay::Wayland(native))
                    .ok_or("EGL initialisation failed")?;
                let window = egl::EglWindow::wayland(display, surface)?;
                slint::platform::femtovg_renderer::FemtoVGRenderer::new(window)
                    .map(|r| Self::OpenGL(Box::new(r)))
                    .map_err(|e| e.to_string())
            }
            #[cfg(feature = "vulkan")]
            RendererKind::Vulkan => {
                let native = gpu::NativeWindow::wayland(&ctx.conn, surface)?;
                gpu::GpuWindow::new(&ctx.gpu, &native).map(|w| Self::Vulkan(Box::new(w)))
            }
            #[allow(unreachable_patterns)]
            other => Err(format!("support for {other} was not compiled in")),
        }
    }

    pub fn kind(&self) -> RendererKind {
        match self {
            Self::Software(_) => RendererKind::Software,
            #[cfg(feature = "opengl")]
            Self::OpenGL(_) => RendererKind::OpenGL,
            #[cfg(feature = "vulkan")]
            Self::Vulkan(_) => RendererKind::Vulkan,
        }
    }

    pub fn as_slint(&self) -> &dyn slint::platform::Renderer {
        match self {
            Self::Software(backend) => &backend.renderer,
            #[cfg(feature = "opengl")]
            Self::OpenGL(renderer) => renderer.as_ref(),
            #[cfg(feature = "vulkan")]
            Self::Vulkan(window) => &window.renderer,
        }
    }

    /// Draws a frame of `width`×`height` physical pixels and commits the surface.
    pub fn render(
        &self,
        ctx: &Context,
        surface: &WlSurface,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        match self {
            Self::Software(backend) => backend.render(&ctx.shm, surface, width, height),
            // eglSwapBuffers attaches and commits.
            #[cfg(feature = "opengl")]
            Self::OpenGL(renderer) => renderer.render().map_err(|e| e.to_string()),
            #[cfg(feature = "vulkan")]
            Self::Vulkan(window) => window.render(width, height),
        }
    }
}
