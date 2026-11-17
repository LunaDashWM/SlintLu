//! Reading the `PanelSettings` struct a `PanelWindow` reports into a [`LayerConfig`].

use slint_interpreter::{ComponentInstance, Struct, Value};
use slintlu::{Anchors, ExclusiveZone, KeyboardMode, Layer, LayerConfig, Margins};

/// What kind of window an exported component is, as reported by its base component.
#[derive(Clone, Debug, PartialEq)]
pub enum WindowKind {
    Panel(PanelSpec),
    Floating {
        app_id: String,
    },
    /// A lock screen, shown on every output while the session is locked.
    Lock,
    /// Not derived from a SlintLu window; the component is not shown.
    Other,
}

/// A panel's layer-shell configuration plus the screens it wants to be on.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelSpec {
    pub layer: LayerConfig,
    pub show_on: Vec<String>,
    pub shown: bool,
}

impl PanelSpec {
    pub fn shows_on(&self, output_name: &str) -> bool {
        self.show_on.is_empty() || self.show_on.iter().any(|name| name == output_name)
    }
}

pub fn window_kind(instance: &ComponentInstance) -> WindowKind {
    let global = |name: &str| instance.get_global_property("SlintLuRuntime", name).ok();
    match global("kind") {
        Some(Value::String(kind)) if kind == "panel" => match global("panel") {
            Some(Value::Struct(settings)) => WindowKind::Panel(spec_from_struct(&settings)),
            _ => WindowKind::Other,
        },
        Some(Value::String(kind)) if kind == "lock" => WindowKind::Lock,
        Some(Value::String(kind)) if kind == "window" => WindowKind::Floating {
            app_id: match global("app-id") {
                Some(Value::String(id)) if !id.is_empty() => id.to_string(),
                _ => "slintlu".into(),
            },
        },
        _ => WindowKind::Other,
    }
}

fn field<'a>(settings: &'a Struct, name: &str) -> Option<&'a Value> {
    settings.get_field(name)
}

fn bool_field(settings: &Struct, name: &str) -> bool {
    matches!(field(settings, name), Some(Value::Bool(true)))
}

fn number_field(settings: &Struct, name: &str) -> f64 {
    match field(settings, name) {
        Some(Value::Number(n)) if n.is_finite() => *n,
        _ => 0.0,
    }
}

fn enum_field<'a>(settings: &'a Struct, name: &str) -> &'a str {
    match field(settings, name) {
        Some(Value::EnumerationValue(_, value)) => value.as_str(),
        _ => "",
    }
}

fn strings_field(settings: &Struct, name: &str) -> Vec<String> {
    use slint::Model;
    match field(settings, name) {
        Some(Value::Model(model)) => model
            .iter()
            .filter_map(|v| match v {
                Value::String(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn spec_from_struct(settings: &Struct) -> PanelSpec {
    let px = |name: &str| number_field(settings, name).round() as i32;
    let layer = match enum_field(settings, "layer") {
        "background" => Layer::Background,
        "bottom" => Layer::Bottom,
        "overlay" => Layer::Overlay,
        _ => Layer::Top,
    };
    let exclusive_zone = match enum_field(settings, "exclusive-mode") {
        "normal" => ExclusiveZone::Normal,
        "ignore" => ExclusiveZone::Ignore,
        "fixed" => ExclusiveZone::Fixed(px("exclusive-zone").max(0) as u32),
        _ => ExclusiveZone::Auto,
    };
    let keyboard = match enum_field(settings, "keyboard-mode") {
        "exclusive" => KeyboardMode::Exclusive,
        "on-demand" => KeyboardMode::OnDemand,
        _ => KeyboardMode::None,
    };
    let namespace = match field(settings, "namespace") {
        Some(Value::String(ns)) if !ns.is_empty() => ns.to_string(),
        _ => "slintlu".into(),
    };
    PanelSpec {
        layer: LayerConfig {
            namespace,
            layer,
            anchors: Anchors {
                top: bool_field(settings, "anchor-top"),
                bottom: bool_field(settings, "anchor-bottom"),
                left: bool_field(settings, "anchor-left"),
                right: bool_field(settings, "anchor-right"),
            },
            exclusive_zone,
            margins: Margins {
                top: px("margin-top"),
                right: px("margin-right"),
                bottom: px("margin-bottom"),
                left: px("margin-left"),
            },
            keyboard,
            background_blur: bool_field(settings, "background-blur"),
            output: None,
        },
        show_on: strings_field(settings, "show-on"),
        shown: !matches!(field(settings, "shown"), Some(Value::Bool(false))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{ModelRc, VecModel};

    fn enum_value(ty: &str, value: &str) -> Value {
        Value::EnumerationValue(ty.into(), value.into())
    }

    fn settings(fields: Vec<(&str, Value)>) -> Struct {
        fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn full_settings_are_converted() {
        let show_on: ModelRc<Value> =
            ModelRc::new(VecModel::from(vec![Value::String("DP-1".into())]));
        let spec = spec_from_struct(&settings(vec![
            ("layer", enum_value("Layer", "overlay")),
            ("namespace", Value::String("launcher".into())),
            ("anchor-top", Value::Bool(true)),
            ("anchor-left", Value::Bool(true)),
            ("exclusive-mode", enum_value("ExclusiveMode", "fixed")),
            ("exclusive-zone", Value::Number(12.4)),
            ("margin-top", Value::Number(8.0)),
            ("keyboard-mode", enum_value("KeyboardMode", "on-demand")),
            ("background-blur", Value::Bool(true)),
            ("show-on", Value::Model(show_on)),
            ("shown", Value::Bool(false)),
        ]));
        assert_eq!(spec.layer.layer, Layer::Overlay);
        assert_eq!(spec.layer.namespace, "launcher");
        assert_eq!(spec.layer.anchors, Anchors { top: true, left: true, ..Anchors::NONE });
        assert_eq!(spec.layer.exclusive_zone, ExclusiveZone::Fixed(12));
        assert_eq!(spec.layer.margins.top, 8);
        assert_eq!(spec.layer.keyboard, KeyboardMode::OnDemand);
        assert!(spec.layer.background_blur);
        assert!(spec.shows_on("DP-1"));
        assert!(!spec.shows_on("HDMI-A-1"));
        assert!(!spec.shown);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let spec = spec_from_struct(&settings(vec![]));
        assert_eq!(spec.layer, LayerConfig::default());
        assert!(spec.shows_on("anything"));
        assert!(spec.shown);
    }
}
