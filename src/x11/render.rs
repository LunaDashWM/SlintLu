//! Renderers for X windows: the GPU renderers through `EGL_EXT_platform_xcb` and Vulkan's
//! XCB surfaces, and CPU rendering into a client-side buffer uploaded with `PutImage`.
//!
//! The software buffer survives between frames, so Slint only repaints and uploads what
//! changed, and `Expose` events are answered without rendering.

use std::cell::{Cell, RefCell};
use std::ops::Range;

use slint::platform::software_renderer::{RepaintBufferType, SoftwareRenderer};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{self, ConnectionExt as _, CreateGCAux, ImageFormat};

use super::context::Context;
#[cfg(feature = "opengl")]
use crate::render::egl;
#[cfg(feature = "vulkan")]
use crate::render::gpu;
use crate::render::RendererKind;
use crate::render::pixel::Argb8888;

pub(crate) enum X11Renderer {
    Software(Box<SoftwareSurface>),
    #[cfg(feature = "opengl")]
    OpenGL(Box<slint::platform::femtovg_renderer::FemtoVGRenderer>),
    #[cfg(feature = "vulkan")]
    Vulkan(Box<gpu::GpuWindow>),
}

impl X11Renderer {
    /// The first renderer of the configured kind's candidates that works for `window`.
    pub fn new(ctx: &Context, window: xproto::Window) -> Result<Self, String> {
        let mut last_error = String::new();
        for &kind in ctx.renderer.candidates() {
            match Self::try_new(kind, ctx, window) {
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
        Err(format!("no usable renderer: {last_error}"))
    }

    fn try_new(kind: RendererKind, ctx: &Context, window: xproto::Window) -> Result<Self, String> {
        match kind {
            RendererKind::Software => {
                Ok(Self::Software(Box::new(SoftwareSurface::new(ctx, window)?)))
            }
            #[cfg(feature = "opengl")]
            RendererKind::OpenGL => {
                let display = egl::EglDisplay::shared(&ctx.egl, ctx.egl_display())
                    .ok_or("EGL initialisation failed")?;
                let surface = egl::EglWindow::xcb(display, window)?;
                slint::platform::femtovg_renderer::FemtoVGRenderer::new(surface)
                    .map(|r| Self::OpenGL(Box::new(r)))
                    .map_err(|e| e.to_string())
            }
            #[cfg(feature = "vulkan")]
            RendererKind::Vulkan => {
                let native = gpu::NativeWindow::xcb(
                    ctx.conn.get_raw_xcb_connection(),
                    ctx.screen as i32,
                    window,
                )?;
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
            Self::Software(surface) => &surface.renderer,
            #[cfg(feature = "opengl")]
            Self::OpenGL(renderer) => renderer.as_ref(),
            #[cfg(feature = "vulkan")]
            Self::Vulkan(window) => &window.renderer,
        }
    }

    /// Draws a frame of `width`×`height` physical pixels.
    pub fn render(
        &self,
        ctx: &Context,
        window: xproto::Window,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        match self {
            Self::Software(surface) => surface.render(ctx, window, width, height),
            #[cfg(feature = "opengl")]
            Self::OpenGL(renderer) => renderer.render().map_err(|e| e.to_string()),
            #[cfg(feature = "vulkan")]
            Self::Vulkan(gpu) => gpu.render(width, height),
        }
    }

    /// Shows the last frame again after an `Expose`. Returns `false` if a new frame must be
    /// rendered instead.
    pub fn redraw(
        &self,
        ctx: &Context,
        window: xproto::Window,
        width: u32,
        height: u32,
    ) -> Result<bool, String> {
        match self {
            Self::Software(surface) => surface.redraw(ctx, window, width, height),
            #[allow(unreachable_patterns)]
            _ => Ok(false),
        }
    }

    /// Frees server-side resources; the window must still exist.
    pub fn destroy(&self, ctx: &Context) {
        if let Self::Software(surface) = self {
            surface.destroy(ctx);
        }
    }
}

/// Bytes of a `PutImage` request besides the image itself.
const PUT_IMAGE_HEADER: usize = 24;

pub(crate) struct SoftwareSurface {
    pub renderer: SoftwareRenderer,
    pixels: RefCell<Vec<Argb8888>>,
    size: Cell<(u32, u32)>,
    gc: xproto::Gcontext,
}

impl SoftwareSurface {
    pub fn new(ctx: &Context, window: xproto::Window) -> Result<Self, String> {
        let gc = ctx.conn.generate_id().map_err(|e| e.to_string())?;
        ctx.conn
            .create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0))
            .map_err(|e| format!("cannot create a graphics context: {e}"))?;
        Ok(Self {
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(
                RepaintBufferType::ReusedBuffer,
            ),
            pixels: RefCell::default(),
            size: Cell::new((0, 0)),
            gc,
        })
    }

    /// Frees the server-side resources; the window must still exist.
    pub fn destroy(&self, ctx: &Context) {
        let _ = ctx.conn.free_gc(self.gc);
    }

    /// Draws a frame of `width`×`height` physical pixels and uploads the changed rows.
    pub fn render(
        &self,
        ctx: &Context,
        window: xproto::Window,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let mut pixels = self.pixels.borrow_mut();
        let resized = self.size.replace((width, height)) != (width, height);
        if resized {
            pixels.clear();
            pixels.resize(width as usize * height as usize, Argb8888::default());
            // The new buffer holds no earlier frame.
            self.renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
        }
        let region = self.renderer.render(&mut pixels, width as usize);
        if resized {
            self.renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        }

        let rows = if resized {
            0..height
        } else {
            let top = region.bounding_box_origin().y.max(0) as u32;
            let bottom = (top + region.bounding_box_size().height).min(height);
            top..bottom
        };
        upload(ctx, window, self.gc, &pixels, width, rows)
    }

    /// Uploads the last frame again, e.g. after an `Expose`. Returns `false` if there is no
    /// frame of this size yet.
    pub fn redraw(
        &self,
        ctx: &Context,
        window: xproto::Window,
        width: u32,
        height: u32,
    ) -> Result<bool, String> {
        if self.size.get() != (width, height) || width == 0 || height == 0 {
            return Ok(false);
        }
        upload(ctx, window, self.gc, &self.pixels.borrow(), width, 0..height)?;
        Ok(true)
    }
}

/// Sends full-width bands of `rows`, as large as the server accepts in one request.
fn upload(
    ctx: &Context,
    window: xproto::Window,
    gc: xproto::Gcontext,
    pixels: &[Argb8888],
    width: u32,
    rows: Range<u32>,
) -> Result<(), String> {
    let stride = width as usize * 4;
    if stride == 0 || rows.is_empty() {
        return Ok(());
    }
    let max_rows = ((ctx.conn.maximum_request_bytes() - PUT_IMAGE_HEADER) / stride).max(1);
    let bytes: &[u8] = bytemuck::cast_slice(pixels);
    let mut y = rows.start as usize;
    while y < rows.end as usize {
        let band = max_rows.min(rows.end as usize - y);
        ctx.conn
            .put_image(
                ImageFormat::Z_PIXMAP,
                window,
                gc,
                width as u16,
                band as u16,
                0,
                y as i16,
                0,
                ctx.visual.depth,
                &bytes[y * stride..(y + band) * stride],
            )
            .map_err(|e| format!("cannot upload the frame: {e}"))?;
        y += band;
    }
    Ok(())
}
