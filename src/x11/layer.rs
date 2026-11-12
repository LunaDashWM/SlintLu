//! Layer surfaces as X11 windows.
//!
//! | Layer | X11 window |
//! |---|---|
//! | `Background` | `_NET_WM_WINDOW_TYPE_DESKTOP`, kept below |
//! | `Bottom` | `_NET_WM_WINDOW_TYPE_DOCK`, kept below |
//! | `Top` | `_NET_WM_WINDOW_TYPE_DOCK`, kept above |
//! | `Overlay` | override-redirect: not managed, above everything |
//!
//! SlintLu places the window itself from the anchors and margins, and an exclusive zone becomes
//! a `_NET_WM_STRUT_PARTIAL`. Lock screens are override-redirect windows covering their output.

use crate::shell::config::{Anchors, Layer, LayerConfig, Margins};

/// A rectangle in physical (X server) pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The screen edge an exclusive zone applies to, as in wlr-layer-shell: the surface must be
/// anchored to that edge, not the opposite one, and to both or neither perpendicular edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

fn exclusive_edge(a: Anchors) -> Option<Edge> {
    if a.top && !a.bottom && a.left == a.right {
        Some(Edge::Top)
    } else if a.bottom && !a.top && a.left == a.right {
        Some(Edge::Bottom)
    } else if a.left && !a.right && a.top == a.bottom {
        Some(Edge::Left)
    } else if a.right && !a.left && a.top == a.bottom {
        Some(Edge::Right)
    } else {
        None
    }
}

fn scaled(margins: Margins, scale: f32) -> Margins {
    let px = |v: i32| (v as f32 * scale).round() as i32;
    Margins {
        top: px(margins.top),
        right: px(margins.right),
        bottom: px(margins.bottom),
        left: px(margins.left),
    }
}

/// Position and length along one axis: stretched between both anchors, pushed to one, or
/// centred.
fn place(anchors: (bool, bool), output: (i32, u32), len: u32, margins: (i32, i32)) -> (i32, u32) {
    let (pos, out_len) = (output.0, output.1 as i32);
    match anchors {
        (true, true) => (pos + margins.0, (out_len - margins.0 - margins.1).max(1) as u32),
        (true, false) => (pos + margins.0, len),
        (false, true) => (pos + out_len - len as i32 - margins.1, len),
        (false, false) => (pos + (out_len - len as i32) / 2, len),
    }
}

/// Where a layer surface whose preferred size is `size` (physical pixels) goes on `output`.
pub(crate) fn geometry(config: &LayerConfig, output: Rect, size: (u32, u32), scale: f32) -> Rect {
    let a = config.anchors;
    let m = scaled(config.margins, scale);
    let (x, width) =
        place((a.left, a.right), (output.x, output.width), size.0.max(1), (m.left, m.right));
    let (y, height) =
        place((a.top, a.bottom), (output.y, output.height), size.1.max(1), (m.top, m.bottom));
    Rect { x, y, width, height }
}

/// The `_NET_WM_STRUT_PARTIAL` value reserving `zone` physical pixels (plus the margin) at the
/// anchored edge of `output`, or `None` when nothing is reserved. Struts are measured from the
/// edges of the whole X screen of size `root`.
pub(crate) fn strut(
    config: &LayerConfig,
    output: Rect,
    root: (u32, u32),
    zone: i32,
    scale: f32,
) -> Option<[u32; 12]> {
    if zone <= 0 {
        return None;
    }
    let m = scaled(config.margins, scale);
    let (x0, x1) = (output.x.max(0) as u32, (output.x + output.width as i32 - 1).max(0) as u32);
    let (y0, y1) = (output.y.max(0) as u32, (output.y + output.height as i32 - 1).max(0) as u32);
    let reserve = |v: i32| v.max(0) as u32;
    let mut s = [0u32; 12];
    match exclusive_edge(config.anchors)? {
        Edge::Left => [s[0], s[4], s[5]] = [reserve(output.x + m.left + zone), y0, y1],
        Edge::Right => {
            let gap = root.0 as i32 - (output.x + output.width as i32);
            [s[1], s[6], s[7]] = [reserve(gap + m.right + zone), y0, y1];
        }
        Edge::Top => [s[2], s[8], s[9]] = [reserve(output.y + m.top + zone), x0, x1],
        Edge::Bottom => {
            let gap = root.1 as i32 - (output.y + output.height as i32);
            [s[3], s[10], s[11]] = [reserve(gap + m.bottom + zone), x0, x1];
        }
    }
    Some(s)
}

/// How the window manager should treat a layer's windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stacking {
    /// `_NET_WM_WINDOW_TYPE_DESKTOP`, below everything.
    Desktop,
    /// `_NET_WM_WINDOW_TYPE_DOCK`, below normal windows.
    DockBelow,
    /// `_NET_WM_WINDOW_TYPE_DOCK`, above normal windows.
    DockAbove,
    /// Override-redirect: the window manager never sees it.
    Unmanaged,
}

pub(crate) fn stacking(layer: Layer) -> Stacking {
    match layer {
        Layer::Background => Stacking::Desktop,
        Layer::Bottom => Stacking::DockBelow,
        Layer::Top => Stacking::DockAbove,
        Layer::Overlay => Stacking::Unmanaged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: Rect = Rect { x: 1920, y: 0, width: 1280, height: 720 };
    const ROOT: (u32, u32) = (3200, 1080);

    fn config(anchors: Anchors) -> LayerConfig {
        LayerConfig { anchors, ..LayerConfig::default() }
    }

    #[test]
    fn bars_stretch_along_their_edge() {
        let bar = config(Anchors::TOP_EDGE);
        assert_eq!(geometry(&bar, OUTPUT, (300, 32), 1.0), Rect { x: 1920, y: 0, width: 1280, height: 32 });
        let bottom = config(Anchors::BOTTOM_EDGE);
        assert_eq!(geometry(&bottom, OUTPUT, (300, 40), 1.0), Rect { x: 1920, y: 680, width: 1280, height: 40 });
        let side = config(Anchors::RIGHT_EDGE);
        assert_eq!(geometry(&side, OUTPUT, (48, 10), 1.0), Rect { x: 3152, y: 0, width: 48, height: 720 });
    }

    #[test]
    fn margins_are_scaled_and_applied() {
        let bar = LayerConfig {
            margins: Margins { top: 4, right: 8, bottom: 0, left: 8 },
            ..config(Anchors::TOP_EDGE)
        };
        assert_eq!(geometry(&bar, OUTPUT, (1, 32), 2.0), Rect { x: 1936, y: 8, width: 1248, height: 32 });
    }

    #[test]
    fn floating_surfaces_are_centred() {
        let popup = config(Anchors::NONE);
        assert_eq!(geometry(&popup, OUTPUT, (400, 200), 1.0), Rect { x: 2360, y: 260, width: 400, height: 200 });
        let corner = config(Anchors { top: true, right: true, ..Anchors::NONE });
        assert_eq!(geometry(&corner, OUTPUT, (100, 50), 1.0), Rect { x: 3100, y: 0, width: 100, height: 50 });
    }

    #[test]
    fn struts_are_relative_to_the_screen_edges() {
        let top = strut(&config(Anchors::TOP_EDGE), OUTPUT, ROOT, 32, 1.0).unwrap();
        assert_eq!((top[2], top[8], top[9]), (32, 1920, 3199));
        // The output ends 360 px above the bottom of the X screen.
        let bottom = strut(&config(Anchors::BOTTOM_EDGE), OUTPUT, ROOT, 40, 1.0).unwrap();
        assert_eq!((bottom[3], bottom[10], bottom[11]), (400, 1920, 3199));
        let left = strut(&config(Anchors::LEFT_EDGE), OUTPUT, ROOT, 48, 1.0).unwrap();
        assert_eq!((left[0], left[4], left[5]), (1968, 0, 719));
    }

    #[test]
    fn no_strut_without_an_exclusive_edge() {
        assert_eq!(strut(&config(Anchors::TOP_EDGE), OUTPUT, ROOT, 0, 1.0), None);
        assert_eq!(strut(&config(Anchors::ALL), OUTPUT, ROOT, 20, 1.0), None);
        let corner = config(Anchors { top: true, left: true, ..Anchors::NONE });
        assert_eq!(strut(&corner, OUTPUT, ROOT, 20, 1.0), None);
    }
}
