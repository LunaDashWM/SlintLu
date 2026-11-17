//! Monitors (`wl_output`): discovery, hot-plug notifications and the public output list.

use std::rc::Rc;

use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};

use crate::core::context::{self, Context};
use crate::core::state::State;

/// A monitor connected to the compositor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    /// Identifier of the output, stable for the lifetime of the connection.
    pub id: u32,
    /// Connector name, e.g. `DP-1` or `eDP-1`.
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    /// Logical position in the global compositor space.
    pub x: i32,
    pub y: i32,
    /// Logical size (after scaling and transform).
    pub width: i32,
    pub height: i32,
    /// Integer scale factor advertised by the output.
    pub scale_factor: i32,
}

pub(crate) fn refresh(ctx: &Context, state: &OutputState) {
    let outputs = state
        .outputs()
        .filter_map(|output| {
            let info = state.info(&output)?;
            let (x, y) = info.logical_position.unwrap_or(info.location);
            let scale = info.scale_factor.max(1);
            let (width, height) = info.logical_size.unwrap_or_else(|| {
                info.modes
                    .iter()
                    .find(|m| m.current)
                    .map(|m| (m.dimensions.0 / scale, m.dimensions.1 / scale))
                    .unwrap_or_default()
            });
            let info = OutputInfo {
                id: output.id().protocol_id(),
                name: info.name.clone().unwrap_or_default(),
                description: info.description.clone().unwrap_or_default(),
                make: info.make.clone(),
                model: info.model.clone(),
                x,
                y,
                width,
                height,
                scale_factor: scale,
            };
            Some((info, output))
        })
        .collect();
    *ctx.outputs.borrow_mut() = outputs;
}

pub(crate) fn wl_output_by_id(ctx: &Context, id: u32) -> Option<WlOutput> {
    ctx.outputs.borrow().iter().find(|(info, _)| info.id == id).map(|(_, o)| o.clone())
}

fn changed(state: &mut State) {
    refresh(&state.ctx, &state.output_state);
    let listeners = state.ctx.output_listeners.borrow().clone();
    for listener in listeners {
        listener();
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        changed(self);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        changed(self);
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        changed(self);
    }
}

/// The outputs currently known to the compositor or X server. Empty before [`crate::init`].
pub fn outputs() -> Vec<OutputInfo> {
    if let Some(ctx) = context::current() {
        return ctx.outputs.borrow().iter().map(|(info, _)| info.clone()).collect();
    }
    #[cfg(feature = "x11")]
    if let Some(ctx) = crate::x11::context::current() {
        return ctx.outputs.borrow().iter().map(|(info, _)| info.clone()).collect();
    }
    Vec::new()
}

/// Registers a callback run on the Slint event loop whenever an output is added, removed or
/// changes (resolution, scale …). Call [`outputs`] inside it to get the new list.
pub fn on_outputs_changed(callback: impl Fn() + 'static) {
    if let Some(ctx) = context::current() {
        ctx.output_listeners.borrow_mut().push(Rc::new(callback));
        return;
    }
    #[cfg(feature = "x11")]
    if let Some(ctx) = crate::x11::context::current() {
        ctx.output_listeners.borrow_mut().push(Rc::new(callback));
    }
}
