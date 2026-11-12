//! Monitors from RandR 1.5 (`GetMonitors`), or the whole screen when RandR is missing, and the
//! scale factor from the `Xft.dpi` resource.

use x11rb::connection::Connection;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};

use super::context::Context;
use super::layer::Rect;
use crate::output::OutputInfo;

/// Subscribes to monitor changes. Returns whether RandR 1.5 is available.
pub(crate) fn watch(conn: &impl Connection, root: xproto::Window) -> bool {
    let supported = conn
        .randr_query_version(1, 5)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 5));
    if !supported {
        log::warn!("RandR 1.5 is not available: the whole X screen is used as one output");
        return false;
    }
    let mask = randr::NotifyMask::SCREEN_CHANGE
        | randr::NotifyMask::CRTC_CHANGE
        | randr::NotifyMask::OUTPUT_CHANGE;
    if let Err(e) = conn.randr_select_input(root, mask) {
        log::warn!("cannot watch monitor changes: {e}");
    }
    true
}

/// `Xft.dpi` / 96, the scale factor desktops configure for X11 applications.
pub(crate) fn read_scale(conn: &impl Connection) -> f32 {
    let dpi = x11rb::resource_manager::new_from_default(conn)
        .ok()
        .and_then(|db| db.get_value::<f32>("Xft.dpi", "").ok().flatten());
    match dpi {
        Some(dpi) if dpi > 0.0 => (dpi / 96.0).clamp(0.5, 8.0),
        _ => 1.0,
    }
}

fn info(id: u32, name: String, rect: Rect, scale: f32) -> (OutputInfo, Rect) {
    let logical = |v: i32| (v as f32 / scale).round() as i32;
    let info = OutputInfo {
        id,
        name,
        description: String::new(),
        make: String::new(),
        model: String::new(),
        x: logical(rect.x),
        y: logical(rect.y),
        width: logical(rect.width as i32),
        height: logical(rect.height as i32),
        scale_factor: scale.round().max(1.0) as i32,
    };
    (info, rect)
}

fn query(ctx: &Context) -> Vec<(OutputInfo, Rect)> {
    let scale = ctx.scale.get();
    if ctx.randr {
        match monitors(ctx, scale) {
            Ok(outputs) if !outputs.is_empty() => return outputs,
            Ok(_) => log::debug!("RandR reports no monitors"),
            Err(e) => log::warn!("cannot list monitors: {e}"),
        }
    }
    let (width, height) = ctx.root_size();
    vec![info(0, "screen".into(), Rect { x: 0, y: 0, width, height }, scale)]
}

fn monitors(
    ctx: &Context,
    scale: f32,
) -> Result<Vec<(OutputInfo, Rect)>, x11rb::errors::ReplyError> {
    let reply = ctx.conn.randr_get_monitors(ctx.root, true)?.reply()?;
    let names: Vec<_> = reply
        .monitors
        .iter()
        .map(|m| ctx.conn.get_atom_name(m.name))
        .collect::<Result<_, _>>()?;
    reply
        .monitors
        .iter()
        .zip(names)
        .map(|(monitor, name)| {
            let rect = Rect {
                x: monitor.x.into(),
                y: monitor.y.into(),
                width: monitor.width.into(),
                height: monitor.height.into(),
            };
            let name = String::from_utf8_lossy(&name.reply()?.name).into_owned();
            // The name atom is unique among monitors and stays the same while the X server
            // runs, also when the monitor is unplugged and plugged in again.
            Ok(info(monitor.name, name, rect, scale))
        })
        .collect()
}

/// Reloads the output list; returns whether it changed.
pub(crate) fn refresh(ctx: &Context) -> bool {
    let outputs = query(ctx);
    let changed = *ctx.outputs.borrow() != outputs;
    *ctx.outputs.borrow_mut() = outputs;
    changed
}

/// Reloads the outputs after a RandR or scale change, moves layer windows to their outputs'
/// new geometry and tells the listeners.
pub(crate) fn changed(ctx: &Context) {
    if !refresh(ctx) {
        return;
    }
    for window in ctx.live_windows() {
        window.outputs_changed();
    }
    let listeners = ctx.output_listeners.borrow().clone();
    for listener in listeners {
        listener();
    }
}
