//! Mapping, unmapping and reacting to compositor configure events.

use slint::LogicalSize;
use slint::platform::WindowEvent;
use smithay_client_toolkit::shell::WaylandSurface;

use super::WaylandWindow;
use crate::shell::config::SurfaceRole;
use crate::shell::{RoleObject, layer, lock, xdg};

impl WaylandWindow {
    /// Gives the surface its role (first time) or re-maps it, then waits for a configure.
    pub(super) fn map(&self) {
        if self.frame.shown.replace(true) {
            return;
        }
        self.frame.configured.set(false);
        let role = self.role.borrow().clone();
        let mut object = self.role_object.borrow_mut();

        if let RoleObject::Unassigned(surface) = &mut *object {
            let Some(surface) = surface.take() else {
                log::error!("window surface was lost, cannot show it again");
                return;
            };
            *object = match &role {
                SurfaceRole::Layer(config) => match layer::create(&self.ctx, &surface, config) {
                    Some(layer) => RoleObject::Layer(layer, surface),
                    None => {
                        log::warn!(
                            "compositor has no layer-shell, showing a normal window instead"
                        );
                        self.create_toplevel(surface, &config.namespace)
                    }
                },
                SurfaceRole::Toplevel { app_id } => self.create_toplevel(surface, app_id),
                SurfaceRole::Lock { output } => match lock::create(&self.ctx, surface, *output) {
                    Ok(lock_surface) => RoleObject::Lock(lock_surface),
                    Err(surface) => {
                        log::error!("cannot show a lock screen: the session is not being locked");
                        RoleObject::Unassigned(Some(surface))
                    }
                },
            };
        }

        match &*object {
            RoleObject::Layer(layer_surface, _) => {
                if let SurfaceRole::Layer(config) = &role {
                    layer::apply(layer_surface, config, self.frame.preferred_size.get(), (0, 0));
                }
                // A commit without a buffer maps the surface; a configure follows.
                self.wl_surface.commit();
            }
            RoleObject::Toplevel(window) => window.commit(),
            // Lock surfaces get their configure without a commit; committing before the first
            // buffer is a protocol error.
            RoleObject::Lock(_) => {}
            RoleObject::Unassigned(_) => {}
        }
    }

    fn create_toplevel(
        &self,
        surface: smithay_client_toolkit::compositor::Surface,
        app_id: &str,
    ) -> RoleObject {
        let title = self.title.borrow().clone();
        match xdg::create(&self.ctx, surface, app_id, &title, self.frame.min_size.get()) {
            Ok(window) => RoleObject::Toplevel(window),
            Err(surface) => {
                log::error!("compositor offers neither layer-shell nor xdg-shell");
                RoleObject::Unassigned(Some(surface))
            }
        }
    }

    /// Unmaps the surface by committing a null buffer. The role object is kept so the window
    /// can be shown again.
    pub(super) fn unmap(&self) {
        if !self.frame.shown.replace(false) {
            return;
        }
        // Lock surfaces must never get a null buffer; they disappear with the lock.
        if matches!(&*self.role_object.borrow(), RoleObject::Lock(_)) {
            return;
        }
        self.frame.configured.set(false);
        self.frame.frame_pending.set(false);
        self.wl_surface.attach(None, 0, 0);
        self.wl_surface.commit();
        // Layer surfaces also lose their role object (see `shell::layer`); showing the window
        // again creates a new one, which also applies a changed output or namespace.
        let mut object = self.role_object.borrow_mut();
        if matches!(&*object, RoleObject::Layer(..)) {
            let RoleObject::Layer(role, surface) =
                std::mem::replace(&mut *object, RoleObject::Unassigned(None))
            else {
                unreachable!()
            };
            drop(role);
            *object = RoleObject::Unassigned(Some(surface));
        }
    }

    pub(crate) fn configure(&self, width: u32, height: u32) {
        let (pref_w, pref_h) = self.frame.preferred_size.get();
        let width = if width == 0 { pref_w.max(1) } else { width };
        let height = if height == 0 { pref_h.max(1) } else { height };
        let resized = self.frame.logical_size.replace((width, height)) != (width, height);
        self.frame.configured.set(true);

        if let (RoleObject::Layer(layer_surface, _), SurfaceRole::Layer(config)) =
            (&*self.role_object.borrow(), &*self.role.borrow())
        {
            // Keep an automatic exclusive zone in sync with the real size.
            layer::set_exclusive_zone(
                layer_surface,
                config.exclusive_zone.resolve(config.anchors, width, height),
            );
            self.update_blur(config.background_blur, (width, height));
        }

        if resized {
            self.dispatch(WindowEvent::Resized {
                size: LogicalSize::new(width as f32, height as f32),
            });
        }
        self.frame.dirty.set(true);
    }

    /// Sends the blur region for the configured size; applied with the next buffer commit.
    fn update_blur(&self, wanted: bool, size: (u32, u32)) {
        let mut blur = self.blur.borrow_mut();
        if wanted {
            let Some(effects) = self.ctx.background_effects.as_ref() else {
                return;
            };
            blur.get_or_insert_with(|| effects.for_surface(&self.wl_surface, &self.ctx.qh))
                .cover(&self.ctx.compositor, size);
        } else if let Some(blur) = blur.as_ref() {
            blur.clear();
        }
    }

    /// Integer scale from `wl_surface.preferred_buffer_scale`; ignored when fractional scaling
    /// is in use.
    pub(crate) fn set_scale_factor(&self, scale: i32) {
        if self.scaling.is_none() {
            self.apply_scale(scale.max(1) as f32);
        }
    }

    /// Scale from `wp_fractional_scale_v1`.
    pub(crate) fn set_fractional_scale(&self, scale: f32) {
        if scale > 0.0 {
            self.apply_scale(scale);
        }
    }

    fn apply_scale(&self, scale: f32) {
        if self.frame.scale.replace(scale) == scale {
            return;
        }
        self.dispatch(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        let (w, h) = self.frame.logical_size.get();
        self.dispatch(WindowEvent::Resized { size: LogicalSize::new(w as f32, h as f32) });
        self.frame.dirty.set(true);
    }

    pub(crate) fn request_close(&self) {
        self.dispatch(WindowEvent::CloseRequested);
    }

    pub(crate) fn closed_by_compositor(&self) {
        log::info!("compositor closed a layer surface");
        self.frame.shown.set(false);
        self.frame.configured.set(false);
        self.dispatch(WindowEvent::CloseRequested);
    }

    pub(super) fn properties_changed(
        &self,
        title: String,
        preferred: (u32, u32),
        min: Option<(u32, u32)>,
    ) {
        let title_changed = *self.title.borrow() != title;
        if title_changed {
            *self.title.borrow_mut() = title.clone();
        }
        let preferred_changed = self.frame.preferred_size.replace(preferred) != preferred;
        self.frame.min_size.set(min);

        match &*self.role_object.borrow() {
            RoleObject::Toplevel(window) => {
                if title_changed {
                    window.set_title(title);
                }
                window.set_min_size(min);
            }
            RoleObject::Layer(..) if preferred_changed => {
                self.role_changed();
            }
            _ => {}
        }
    }

    /// Re-sends the layer configuration of a mapped layer surface.
    pub(super) fn role_changed(&self) {
        if !self.frame.shown.get() {
            return;
        }
        if let (RoleObject::Layer(layer_surface, _), SurfaceRole::Layer(config)) =
            (&*self.role_object.borrow(), &*self.role.borrow())
        {
            layer::apply(
                layer_surface,
                config,
                self.frame.preferred_size.get(),
                self.frame.logical_size.get(),
            );
            self.wl_surface.commit();
        }
    }
}
