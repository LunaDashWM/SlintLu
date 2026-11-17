//! Plain data types describing how a Slint window is mapped on a Wayland compositor.

/// The wlr-layer-shell layer a surface lives on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    Background,
    Bottom,
    #[default]
    Top,
    Overlay,
}

/// Screen edges a layer surface is anchored to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anchors {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl Anchors {
    pub const NONE: Self = Self { top: false, bottom: false, left: false, right: false };
    pub const ALL: Self = Self { top: true, bottom: true, left: true, right: true };
    /// A full-width bar along the top of the screen.
    pub const TOP_EDGE: Self = Self { top: true, bottom: false, left: true, right: true };
    /// A full-width bar along the bottom of the screen.
    pub const BOTTOM_EDGE: Self = Self { top: false, bottom: true, left: true, right: true };
    /// A full-height panel along the left side of the screen.
    pub const LEFT_EDGE: Self = Self { top: true, bottom: true, left: true, right: false };
    /// A full-height panel along the right side of the screen.
    pub const RIGHT_EDGE: Self = Self { top: true, bottom: true, left: false, right: true };

    /// The surface is stretched horizontally by the compositor.
    pub fn stretches_horizontally(self) -> bool {
        self.left && self.right
    }

    /// The surface is stretched vertically by the compositor.
    pub fn stretches_vertically(self) -> bool {
        self.top && self.bottom
    }
}

/// Space a layer surface reserves at the edge it is anchored to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExclusiveZone {
    /// Reserve the surface's own height (or width, for vertical panels) when it is anchored to
    /// a single edge, nothing otherwise.
    #[default]
    Auto,
    /// Do not reserve space, but let other exclusive zones push this surface around.
    Normal,
    /// Ignore other surfaces' exclusive zones (e.g. wallpapers, full-screen overlays).
    Ignore,
    /// Reserve exactly this many logical pixels.
    Fixed(u32),
}

impl ExclusiveZone {
    /// The value to send to the compositor for a surface of the given logical size.
    pub(crate) fn resolve(self, anchors: Anchors, width: u32, height: u32) -> i32 {
        match self {
            ExclusiveZone::Normal => 0,
            ExclusiveZone::Ignore => -1,
            ExclusiveZone::Fixed(px) => px.min(i32::MAX as u32) as i32,
            ExclusiveZone::Auto => {
                let horizontal_bar =
                    anchors.stretches_horizontally() && (anchors.top ^ anchors.bottom);
                let vertical_bar = anchors.stretches_vertically() && (anchors.left ^ anchors.right);
                let single_edge = [anchors.top, anchors.bottom, anchors.left, anchors.right]
                    .iter()
                    .filter(|a| **a)
                    .count()
                    == 1;
                if horizontal_bar || (single_edge && (anchors.top || anchors.bottom)) {
                    height as i32
                } else if vertical_bar || (single_edge && (anchors.left || anchors.right)) {
                    width as i32
                } else {
                    0
                }
            }
        }
    }
}

/// Distance in logical pixels from the anchored edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Margins {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

/// How a layer surface receives keyboard focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyboardMode {
    /// Never receives keyboard input (bars, wallpapers).
    #[default]
    None,
    /// Grabs the keyboard while mapped (launchers, lock screens on the overlay layer).
    Exclusive,
    /// Receives focus when clicked, like a normal window.
    OnDemand,
}

/// Everything needed to create a `zwlr_layer_surface_v1`.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerConfig {
    /// Namespace reported to the compositor, used e.g. for layer rules.
    pub namespace: String,
    pub layer: Layer,
    pub anchors: Anchors,
    pub exclusive_zone: ExclusiveZone,
    pub margins: Margins,
    pub keyboard: KeyboardMode,
    /// Ask the compositor to blur what is behind the surface (`ext-background-effect-v1`).
    pub background_blur: bool,
    /// Wayland output (see [`crate::OutputInfo::id`]) to place the surface on, or the
    /// compositor's choice when `None`.
    ///
    /// The output is fixed once the window has been shown for the first time.
    pub output: Option<u32>,
}

impl Default for LayerConfig {
    fn default() -> Self {
        Self {
            namespace: "slintlu".into(),
            layer: Layer::Top,
            anchors: Anchors::NONE,
            exclusive_zone: ExclusiveZone::Auto,
            margins: Margins::default(),
            keyboard: KeyboardMode::None,
            background_blur: false,
            output: None,
        }
    }
}

/// The Wayland role given to a window's surface when it is shown.
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceRole {
    /// A regular `xdg_toplevel` application window.
    Toplevel {
        /// The `app_id` reported to the compositor.
        app_id: String,
    },
    /// A `wlr-layer-shell` surface.
    Layer(LayerConfig),
    /// A lock screen surface covering one output while the session is locked (see
    /// [`crate::security::lock_session`]).
    Lock {
        /// The output (see [`crate::OutputInfo::id`]) to cover.
        output: u32,
    },
}

impl Default for SurfaceRole {
    fn default() -> Self {
        SurfaceRole::Toplevel { app_id: "slintlu".into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_exclusive_zone_reserves_bar_thickness() {
        let auto = ExclusiveZone::Auto;
        assert_eq!(auto.resolve(Anchors::TOP_EDGE, 1920, 32), 32);
        assert_eq!(auto.resolve(Anchors::BOTTOM_EDGE, 1920, 40), 40);
        assert_eq!(auto.resolve(Anchors::LEFT_EDGE, 48, 1080), 48);
        assert_eq!(auto.resolve(Anchors::RIGHT_EDGE, 64, 1080), 64);
        // A single edge: still a bar.
        let top_only = Anchors { top: true, ..Anchors::NONE };
        assert_eq!(auto.resolve(top_only, 300, 30), 30);
        // Floating or full-screen surfaces reserve nothing.
        assert_eq!(auto.resolve(Anchors::NONE, 300, 300), 0);
        assert_eq!(auto.resolve(Anchors::ALL, 1920, 1080), 0);
    }

    #[test]
    fn explicit_exclusive_zones() {
        assert_eq!(ExclusiveZone::Ignore.resolve(Anchors::TOP_EDGE, 10, 10), -1);
        assert_eq!(ExclusiveZone::Normal.resolve(Anchors::TOP_EDGE, 10, 10), 0);
        assert_eq!(ExclusiveZone::Fixed(12).resolve(Anchors::NONE, 10, 10), 12);
    }
}
