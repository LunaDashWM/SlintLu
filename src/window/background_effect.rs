//! `ext-background-effect-v1`: asks the compositor to blur what is behind a panel.
//!
//! The blur region is double-buffered `wl_surface` state, so it is sent with the size of each
//! configure and applied with the next buffer commit. How the blur looks is the compositor's
//! choice; compositors without the protocol simply show the panel as drawn.

use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};

use super::scale::NoEvents;
use crate::core::state::State;

/// The bound `ext_background_effect_manager_v1` global.
pub(crate) struct BackgroundEffects {
    manager: ExtBackgroundEffectManagerV1,
}

/// A surface's background effect object, destroyed with the window.
///
/// A `wl_surface` may have only one; creating a second is a protocol error, so a window keeps
/// this object while it is hidden and shown again.
pub(crate) struct SurfaceBlur {
    effect: ExtBackgroundEffectSurfaceV1,
}

impl Drop for SurfaceBlur {
    fn drop(&mut self) {
        self.effect.destroy();
    }
}

impl BackgroundEffects {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<Self> {
        match globals.bind::<ExtBackgroundEffectManagerV1, State, NoEvents>(qh, 1..=1, NoEvents) {
            Ok(manager) => Some(Self { manager }),
            Err(_) => {
                log::info!("ext-background-effect-v1 unavailable: panels are not blurred");
                None
            }
        }
    }

    pub fn for_surface(&self, surface: &WlSurface, qh: &QueueHandle<State>) -> SurfaceBlur {
        SurfaceBlur { effect: self.manager.get_background_effect(surface, qh, NoEvents) }
    }
}

impl SurfaceBlur {
    /// Blurs the whole surface of the given logical size. Takes effect with the next
    /// `wl_surface.commit`.
    pub fn cover(&self, compositor: &CompositorState, (width, height): (u32, u32)) {
        match Region::new(compositor) {
            Ok(region) => {
                let clamp = |v: u32| v.min(i32::MAX as u32) as i32;
                region.add(0, 0, clamp(width), clamp(height));
                self.effect.set_blur_region(Some(region.wl_region()));
            }
            Err(e) => log::warn!("cannot create a blur region: {e}"),
        }
    }

    /// Removes the blur with the next `wl_surface.commit`.
    pub fn clear(&self) {
        self.effect.set_blur_region(None);
    }
}

impl Dispatch2<ExtBackgroundEffectManagerV1, State> for NoEvents {
    fn event(
        &self,
        _: &mut State,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // The compositor itself stops applying the blur when the capability goes away.
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            log::debug!("background effect capabilities: {flags:?}");
        }
    }
}

impl Dispatch2<ExtBackgroundEffectSurfaceV1, State> for NoEvents {
    fn event(
        &self,
        _: &mut State,
        _: &ExtBackgroundEffectSurfaceV1,
        _: ext_background_effect_surface_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
