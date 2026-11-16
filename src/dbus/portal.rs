//! Appearance settings (dark mode, accent colour, interface font) from the XDG desktop portal.

use futures_util::StreamExt;
use zbus::{Connection, OwnedVariant, Variant, proxy};

const APPEARANCE: &str = "org.freedesktop.appearance";
// The desktop's interface font, as GNOME-compatible portal backends publish it.
const INTERFACE: &str = "org.gnome.desktop.interface";
const FONT_NAME: &str = "font-name";

#[proxy(
    interface = "org.freedesktop.portal.Settings",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Settings {
    fn read_one(&self, namespace: &str, key: &str) -> zbus::Result<OwnedVariant>;
    #[zbus(signal)]
    fn setting_changed(&self, namespace: &str, key: &str, value: Variant<'_>) -> zbus::Result<()>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorScheme {
    #[default]
    NoPreference,
    Dark,
    Light,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Appearance {
    pub color_scheme: ColorScheme,
    /// sRGB in 0..1, if the desktop provides one.
    pub accent: Option<(f32, f32, f32)>,
    pub high_contrast: bool,
    /// The family of the desktop's interface font, if the desktop provides one.
    pub font_family: Option<String>,
}

/// The family of a Pango font description such as "Adwaita Sans 11" or
/// "Noto Sans CJK TC, Bold 10": the first family, without style words and size.
pub fn font_description_family(description: &str) -> Option<String> {
    const STYLES: &[&str] = &[
        "normal",
        "roman",
        "oblique",
        "italic",
        "small-caps",
        "thin",
        "ultra-light",
        "extra-light",
        "light",
        "semi-light",
        "demi-light",
        "book",
        "regular",
        "medium",
        "semi-bold",
        "demi-bold",
        "bold",
        "ultra-bold",
        "extra-bold",
        "heavy",
        "black",
        "ultra-heavy",
        "extra-heavy",
        "ultra-condensed",
        "extra-condensed",
        "condensed",
        "semi-condensed",
        "semi-expanded",
        "expanded",
        "extra-expanded",
        "ultra-expanded",
    ];
    let mut words: Vec<&str> = description.split_whitespace().collect();
    if words.last().is_some_and(|w| w.trim_end_matches("px").parse::<f64>().is_ok()) {
        words.pop();
    }
    while words.last().is_some_and(|w| {
        let w = w.trim_end_matches(',').to_ascii_lowercase();
        STYLES.contains(&w.as_str())
    }) {
        words.pop();
    }
    let families = words.join(" ");
    let family = families.split(',').next().unwrap_or_default().trim();
    (!family.is_empty()).then(|| family.to_owned())
}

fn color_scheme(value: &Variant<'_>) -> ColorScheme {
    match u32::try_from(value) {
        Ok(1) => ColorScheme::Dark,
        Ok(2) => ColorScheme::Light,
        _ => ColorScheme::NoPreference,
    }
}

fn accent(value: &Variant<'_>) -> Option<(f32, f32, f32)> {
    let (r, g, b) = <(f64, f64, f64)>::try_from(value).ok()?;
    let valid = |c: f64| (0.0..=1.0).contains(&c);
    (valid(r) && valid(g) && valid(b)).then_some((r as f32, g as f32, b as f32))
}

fn apply(appearance: &mut Appearance, namespace: &str, key: &str, value: &Variant<'_>) -> bool {
    if namespace == INTERFACE {
        if key != FONT_NAME {
            return false;
        }
        appearance.font_family = <&str>::try_from(value).ok().and_then(font_description_family);
        return true;
    }
    if namespace != APPEARANCE {
        return false;
    }
    match key {
        "color-scheme" => appearance.color_scheme = color_scheme(value),
        "accent-color" => appearance.accent = accent(value),
        "contrast" => appearance.high_contrast = u32::try_from(value) == Ok(1),
        _ => return false,
    }
    true
}

#[derive(Clone)]
pub struct Portal {
    connection: Connection,
}

impl Portal {
    pub fn connect() -> zbus::Result<Self> {
        Ok(Self { connection: zbus::block_on(Connection::session())? })
    }

    async fn read(&self) -> zbus::Result<Appearance> {
        let settings = SettingsProxy::new(&self.connection).await?;
        let mut appearance = Appearance::default();
        for (namespace, key) in [
            (APPEARANCE, "color-scheme"),
            (APPEARANCE, "accent-color"),
            (APPEARANCE, "contrast"),
            (INTERFACE, FONT_NAME),
        ] {
            if let Ok(value) = settings.read_one(namespace, key).await {
                apply(&mut appearance, namespace, key, &value);
            }
        }
        Ok(appearance)
    }

    pub fn appearance(&self) -> zbus::Result<Appearance> {
        zbus::block_on(self.read())
    }

    /// Blocks, calling `on_change` initially and whenever an appearance setting changes.
    pub fn watch(&self, mut on_change: impl FnMut(&Appearance)) -> zbus::Result<()> {
        zbus::block_on(async {
            let settings = SettingsProxy::new(&self.connection).await?;
            let mut changes = settings.receive_setting_changed().await?;
            let mut appearance = self.read().await?;
            on_change(&appearance);
            while let Some(signal) = changes.next().await {
                let Ok(args) = signal.args() else { continue };
                if apply(&mut appearance, args.namespace, args.key, &args.value) {
                    on_change(&appearance);
                }
            }
            Err(zbus::Error::Failure("the session bus closed".into()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_settings() {
        let mut appearance = Appearance::default();
        assert!(apply(&mut appearance, APPEARANCE, "color-scheme", &Variant::from(1u32)));
        assert_eq!(appearance.color_scheme, ColorScheme::Dark);
        let accent = Variant::from((0.2f64, 0.4f64, 1.0f64));
        assert!(apply(&mut appearance, APPEARANCE, "accent-color", &accent));
        assert_eq!(appearance.accent, Some((0.2, 0.4, 1.0)));
        let invalid = Variant::from((-1.0f64, 0.0f64, 0.0f64));
        assert!(apply(&mut appearance, APPEARANCE, "accent-color", &invalid));
        assert_eq!(appearance.accent, None);
        assert!(!apply(&mut appearance, APPEARANCE, "reduced-motion", &Variant::from(1u32)));
        assert!(apply(&mut appearance, INTERFACE, FONT_NAME, &Variant::from("Adwaita Sans 11")));
        assert_eq!(appearance.font_family.as_deref(), Some("Adwaita Sans"));
        assert!(!apply(&mut appearance, INTERFACE, "cursor-size", &Variant::from(24i32)));
    }

    #[test]
    fn reads_font_families() {
        assert_eq!(font_description_family("Adwaita Sans 11").as_deref(), Some("Adwaita Sans"));
        assert_eq!(
            font_description_family("Noto Sans CJK TC, Bold Italic 10.5").as_deref(),
            Some("Noto Sans CJK TC")
        );
        assert_eq!(font_description_family("Cantarell").as_deref(), Some("Cantarell"));
        assert_eq!(font_description_family("12"), None);
    }
}
