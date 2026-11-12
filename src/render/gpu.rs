//! Vulkan rendering through wgpu, for Slint's FemtoVG wgpu renderer.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use slint::platform::femtovg_renderer::FemtoVGWGPURenderer;
use slint::wgpu_30::wgpu;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Proxy};

/// The shared wgpu device of a platform: `None` until first use, `Some(None)` if
/// initialisation failed.
pub(crate) type SharedGpu = RefCell<Option<Option<Rc<GpuContext>>>>;

/// The native window a wgpu surface presents to. It must outlive the [`GpuWindow`].
pub(crate) struct NativeWindow {
    display: RawDisplayHandle,
    window: RawWindowHandle,
}

impl NativeWindow {
    pub fn wayland(conn: &Connection, surface: &WlSurface) -> Result<Self, String> {
        let display =
            NonNull::new(conn.backend().display_ptr() as *mut c_void).ok_or("null wl_display")?;
        let window = NonNull::new(surface.id().as_ptr() as *mut c_void).ok_or("null wl_surface")?;
        Ok(Self {
            display: RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display)),
            window: RawWindowHandle::Wayland(WaylandWindowHandle::new(window)),
        })
    }

    #[cfg(feature = "x11")]
    pub fn xcb(connection: *mut c_void, screen: i32, window: u32) -> Result<Self, String> {
        use raw_window_handle::{XcbDisplayHandle, XcbWindowHandle};
        let connection = NonNull::new(connection).ok_or("null xcb connection")?;
        let window = std::num::NonZeroU32::new(window).ok_or("null X window")?;
        Ok(Self {
            display: RawDisplayHandle::Xcb(XcbDisplayHandle::new(Some(connection), screen)),
            window: RawWindowHandle::Xcb(XcbWindowHandle::new(window)),
        })
    }
}

/// Instance, adapter, device and queue shared by every window.
pub(crate) struct GpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn create_surface(
    instance: &wgpu::Instance,
    native: &NativeWindow,
) -> Result<wgpu::Surface<'static>, String> {
    let target = wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(native.display),
        raw_window_handle: native.window,
    };
    // SAFETY: the connection lives as long as the platform, and windows drop their
    // `GpuWindow` before the native window (field order in `WaylandWindow` and `X11Window`).
    unsafe { instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())
}

impl GpuContext {
    /// Returns the shared context, creating it (and the first window's surface) on first use.
    fn shared(
        cache: &SharedGpu,
        native: &NativeWindow,
    ) -> Result<(Rc<Self>, wgpu::Surface<'static>), String> {
        let cached = cache.borrow().clone();
        if let Some(gpu) = cached {
            let gpu = gpu.ok_or("Vulkan initialisation failed earlier")?;
            let wgpu_surface = create_surface(&gpu.instance, native)?;
            return Ok((gpu, wgpu_surface));
        }
        let result = Self::new(native);
        *cache.borrow_mut() = Some(result.as_ref().ok().map(|(gpu, _)| gpu.clone()));
        result
    }

    fn new(native: &NativeWindow) -> Result<(Rc<Self>, wgpu::Surface<'static>), String> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let wgpu_surface = create_surface(&instance, native)?;

        // Prefer a real GPU; fall back to a software implementation (e.g. lavapipe in virtual
        // machines and CI) when no hardware adapter can present to this surface.
        let request = |force_fallback_adapter| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&wgpu_surface),
                force_fallback_adapter,
                ..Default::default()
            }))
        };
        let adapter = request(false)
            .or_else(|e| {
                log::debug!("no hardware Vulkan adapter ({e}), trying a fallback adapter");
                request(true)
            })
            .map_err(|e| format!("no Vulkan adapter: {e}"))?;
        let info = adapter.get_info();
        log::info!(
            "Vulkan adapter: {} ({:?}, driver {})",
            info.name,
            info.device_type,
            info.driver
        );

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("slintlu"),
            ..Default::default()
        }))
        .map_err(|e| format!("cannot open the Vulkan device: {e}"))?;
        // wgpu panics on validation errors by default; a shell must keep running instead.
        device.on_uncaptured_error(std::sync::Arc::new(|error| {
            log::error!("wgpu error: {error}");
        }));

        Ok((Rc::new(Self { instance, adapter, device, queue }), wgpu_surface))
    }
}

/// Lists Vulkan adapters (for diagnostics).
pub(crate) fn probe() -> Result<String, String> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    descriptor.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(descriptor);
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
    if adapters.is_empty() {
        return Err("no Vulkan adapter (is a Vulkan driver installed?)".into());
    }
    Ok(adapters
        .iter()
        .map(|a| {
            let info = a.get_info();
            format!("{} ({:?})", info.name, info.device_type)
        })
        .collect::<Vec<_>>()
        .join(", "))
}

/// One window's swapchain plus its FemtoVG renderer.
pub(crate) struct GpuWindow {
    pub renderer: FemtoVGWGPURenderer,
    surface: wgpu::Surface<'static>,
    gpu: Rc<GpuContext>,
    configured_size: Cell<(u32, u32)>,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    present_mode: wgpu::PresentMode,
}

impl GpuWindow {
    pub fn new(cache: &SharedGpu, native: &NativeWindow) -> Result<Self, String> {
        let (gpu, surface) = GpuContext::shared(cache, native)?;
        let caps = surface.get_capabilities(&gpu.adapter);
        // FemtoVG blends in non-sRGB space; prefer a plain 8-bit format.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm)
            })
            .or_else(|| caps.formats.first().copied())
            .ok_or("surface reports no formats")?;
        let alpha_mode =
            [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::Inherit]
                .into_iter()
                .find(|m| caps.alpha_modes.contains(m))
                .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        // Mailbox never blocks; FIFO is always available.
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let renderer =
            FemtoVGWGPURenderer::new(gpu.instance.clone(), gpu.device.clone(), gpu.queue.clone())
                .map_err(|e| e.to_string())?;
        Ok(Self {
            renderer,
            surface,
            gpu,
            configured_size: Cell::new((0, 0)),
            format,
            alpha_mode,
            present_mode,
        })
    }

    fn configure(&self, width: u32, height: u32) {
        self.surface.configure(
            &self.gpu.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                width,
                height,
                present_mode: self.present_mode,
                desired_maximum_frame_latency: 2,
                alpha_mode: self.alpha_mode,
                color_space: wgpu::SurfaceColorSpace::Auto,
                view_formats: vec![],
            },
        );
        self.configured_size.set((width, height));
    }

    fn acquire(&self, width: u32, height: u32) -> Option<wgpu::SurfaceTexture> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => Some(t),
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                log::debug!("suboptimal swapchain at {width}x{height}");
                Some(t)
            }
            other => {
                log::debug!("swapchain image unavailable: {other:?}");
                None
            }
        }
    }

    pub fn render(&self, width: u32, height: u32) -> Result<(), String> {
        if self.configured_size.get() != (width, height) {
            self.configure(width, height);
        }
        let texture = match self.acquire(width, height) {
            Some(texture) => texture,
            // Reconfigure once (resizes make the swapchain outdated) and try again.
            None => {
                self.configure(width, height);
                self.acquire(width, height).ok_or("cannot acquire a swapchain image")?
            }
        };
        self.renderer.render_to_texture(&texture.texture).map_err(|e| e.to_string())?;
        self.gpu.queue.present(texture);
        Ok(())
    }
}
