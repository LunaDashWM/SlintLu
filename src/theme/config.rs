//! `theme.json`: which palette to use and how to override it.
//!
//! ```json
//! {
//!   "dark": "tokyo-night",
//!   "light": "catppuccin-latte",
//!   "mode": "auto",
//!   "system-accent": true,
//!   "pywal": false,
//!   "colors": { "accent": "#ff79c6" },
//!   "font-family": "Inter",
//!   "font-size": 13,
//!   "radius": 10
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::palette::{Color, Palette, preset};
use super::pywal;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ThemeConfig {
    /// Palette for dark mode.
    pub dark: String,
    /// Palette for light mode.
    pub light: String,
    /// "auto" (follow the desktop), "dark" or "light".
    pub mode: String,
    /// Use the desktop's accent colour when it has one.
    pub system_accent: bool,
    /// Use pywal / wallust colours when available.
    pub pywal: bool,
    /// Role → `#rrggbb` overrides, e.g. `"accent"`.
    pub colors: HashMap<String, Color>,
    /// Extra named palettes, usable as `dark` / `light`.
    pub palettes: HashMap<String, Palette>,
    pub font_family: String,
    pub font_size: f32,
    pub radius: f32,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            dark: "catppuccin-mocha".into(),
            light: "catppuccin-latte".into(),
            mode: "auto".into(),
            system_accent: true,
            pywal: false,
            colors: HashMap::new(),
            palettes: HashMap::new(),
            font_family: String::new(),
            font_size: 13.0,
            radius: 10.0,
        }
    }
}

impl ThemeConfig {
    /// Reads `theme.json`; a missing file gives the defaults, a broken one is reported.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(_) => Ok(Self::default()),
        }
    }
}

fn apply_override(palette: &mut Palette, role: &str, color: Color) {
    let slot = match role {
        "background" => &mut palette.background,
        "surface" => &mut palette.surface,
        "surface-alt" => &mut palette.surface_alt,
        "border" => &mut palette.border,
        "text" => &mut palette.text,
        "text-muted" => &mut palette.text_muted,
        "accent" => &mut palette.accent,
        "red" => &mut palette.red,
        "green" => &mut palette.green,
        "yellow" => &mut palette.yellow,
        "blue" => &mut palette.blue,
        "magenta" => &mut palette.magenta,
        "cyan" => &mut palette.cyan,
        other => return log::warn!("theme.json: unknown colour role `{other}`"),
    };
    *slot = color;
}

/// Whether the mode selects the `dark` palette slot: "dark", "light", or else the desktop's
/// preference (dark without one). The chosen palette itself may be light either way.
pub fn uses_dark_slot(config: &ThemeConfig, desktop_prefers_dark: Option<bool>) -> bool {
    match config.mode.as_str() {
        "dark" => true,
        "light" => false,
        _ => desktop_prefers_dark.unwrap_or(true),
    }
}

/// Puts a palette in the slot the current mode reads, so choosing it always shows it.
pub fn choose_palette(config: &mut ThemeConfig, name: String, desktop_prefers_dark: Option<bool>) {
    config.pywal = false;
    if uses_dark_slot(config, desktop_prefers_dark) {
        config.dark = name;
    } else {
        config.light = name;
    }
}

/// The palette to use given the configuration, the desktop's dark-mode preference and accent.
pub fn resolve(
    config: &ThemeConfig,
    desktop_prefers_dark: Option<bool>,
    desktop_accent: Option<Color>,
) -> Palette {
    let dark = uses_dark_slot(config, desktop_prefers_dark);
    let named = if dark { &config.dark } else { &config.light };
    let fallback = if dark { "catppuccin-mocha" } else { "catppuccin-latte" };
    let mut palette = config
        .pywal
        .then(|| pywal::load(&pywal::default_path()))
        .flatten()
        .or_else(|| {
            config.palettes.get(named).cloned().map(|p| Palette { name: named.clone(), ..p })
        })
        .or_else(|| preset(named))
        .unwrap_or_else(|| {
            log::warn!("unknown theme `{named}`, using {fallback}");
            preset(fallback).expect("built-in palette")
        });
    if config.system_accent
        && let Some(accent) = desktop_accent
    {
        palette.accent = accent;
    }
    for (role, color) in &config.colors {
        apply_override(&mut palette, role, *color);
    }
    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_desktop_unless_forced() {
        let config = ThemeConfig::default();
        assert_eq!(resolve(&config, Some(false), None).name, "catppuccin-latte");
        assert_eq!(resolve(&config, Some(true), None).name, "catppuccin-mocha");
        assert_eq!(resolve(&config, None, None).name, "catppuccin-mocha");
        let forced = ThemeConfig { mode: "light".into(), ..ThemeConfig::default() };
        assert_eq!(resolve(&forced, Some(true), None).name, "catppuccin-latte");
    }

    #[test]
    fn accents_and_overrides() {
        let accent = Color(1, 2, 3);
        let config = ThemeConfig::default();
        assert_eq!(resolve(&config, Some(true), Some(accent)).accent, accent);
        let no_system = ThemeConfig { system_accent: false, ..ThemeConfig::default() };
        assert_ne!(resolve(&no_system, Some(true), Some(accent)).accent, accent);
        let json = r##"{"dark": "nord", "colors": {"accent": "#ff0000", "text": "#ffffff"}, "radius": 4}"##;
        let config: ThemeConfig = serde_json::from_str(json).unwrap();
        let palette = resolve(&config, Some(true), Some(accent));
        assert_eq!(palette.name, "nord");
        assert_eq!(palette.accent, Color(255, 0, 0));
        assert_eq!(palette.text, Color(255, 255, 255));
        assert_eq!(config.radius, 4.0);
    }

    #[test]
    fn custom_palettes() {
        let json = r##"{"dark": "mine", "palettes": {"mine": {"dark": true, "background": "#111318",
            "surface": "#191c22", "surface-alt": "#22262e", "border": "#586172", "text": "#e2e5ed",
            "text-muted": "#bdc5d3", "accent": "#9ccbfb", "red": "#ffb4ab", "green": "#b4d7ad",
            "yellow": "#efcd8c", "blue": "#9ccbfb", "magenta": "#bbc7d9", "cyan": "#a8e0e0"}}}"##;
        let config: ThemeConfig = serde_json::from_str(json).unwrap();
        let palette = resolve(&config, Some(true), None);
        assert_eq!(palette.name, "mine");
        assert_eq!(palette.background, Color(0x11, 0x13, 0x18));
    }

    #[test]
    fn every_chosen_palette_shows() {
        // A light palette in dark mode must not move later choices to the light slot.
        let mut config = ThemeConfig { mode: "dark".into(), ..ThemeConfig::default() };
        choose_palette(&mut config, "gruvbox-light".into(), None);
        assert_eq!(resolve(&config, None, None).name, "gruvbox-light");
        choose_palette(&mut config, "nord".into(), None);
        assert_eq!(resolve(&config, None, None).name, "nord");
        let mut auto = ThemeConfig::default();
        choose_palette(&mut auto, "nord".into(), Some(false));
        assert_eq!(resolve(&auto, Some(false), None).name, "nord");
    }

    #[test]
    fn unknown_palettes_fall_back() {
        let config = ThemeConfig { dark: "missing".into(), ..ThemeConfig::default() };
        assert_eq!(resolve(&config, Some(true), None).name, "catppuccin-mocha");
    }
}
