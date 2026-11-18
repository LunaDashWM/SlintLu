//! The `Theme` global: palettes from `theme.json`, pywal and the desktop portal. The font
//! family is the one chosen with `set-font-family`, else `theme.json`'s, else the desktop's
//! interface font; `fonts` / `search-fonts` list the installed families.

use std::cell::RefCell;
use std::rc::Rc;

use slint::Color as SlintColor;
use slint_interpreter::Value;
use slintlu::dbus::portal::{Appearance, ColorScheme, Portal};
use slintlu::theme::{self, Color, ThemeConfig, fonts};

use super::worker::spawn_watcher;
use crate::scripting::bridge::{list_value, number_arg, string, string_arg};
use crate::scripting::{ShellHost, host};

#[derive(Default)]
struct ThemeState {
    config: ThemeConfig,
    appearance: Option<Appearance>,
    /// Chosen at run time (`set-font-family`); wins over theme.json.
    font_family: Option<String>,
    fonts: Vec<String>,
}

thread_local! {
    static STATE: RefCell<ThemeState> = RefCell::default();
}

fn color(c: Color) -> Value {
    Value::Brush(SlintColor::from_rgb_u8(c.0, c.1, c.2).into())
}

/// The desktop portal's dark-mode preference, if it states one.
fn desktop_prefers_dark(state: &ThemeState) -> Option<bool> {
    state.appearance.as_ref().and_then(|a| match a.color_scheme {
        ColorScheme::Dark => Some(true),
        ColorScheme::Light => Some(false),
        ColorScheme::NoPreference => None,
    })
}

fn publish(host: &ShellHost) {
    STATE.with(|state| {
        let state = state.borrow();
        let prefers_dark = desktop_prefers_dark(&state);
        let accent = state.appearance.as_ref().and_then(|a| a.accent).map(|(r, g, b)| {
            let c = |v: f32| (v * 255.0).round() as u8;
            Color(c(r), c(g), c(b))
        });
        let p = theme::resolve(&state.config, prefers_dark, accent);
        let set = |name: &str, value: Value| host.set_global("Theme", name, value);
        set("name", string(&p.name));
        set("dark", Value::Bool(p.dark));
        set("has-preference", Value::Bool(prefers_dark.is_some()));
        set(
            "high-contrast",
            Value::Bool(state.appearance.as_ref().is_some_and(|a| a.high_contrast)),
        );
        for (role, c) in [
            ("background", p.background),
            ("surface", p.surface),
            ("surface-alt", p.surface_alt),
            ("border", p.border),
            ("text", p.text),
            ("text-muted", p.text_muted),
            ("accent", p.accent),
            ("red", p.red),
            ("green", p.green),
            ("yellow", p.yellow),
            ("blue", p.blue),
            ("magenta", p.magenta),
            ("cyan", p.cyan),
        ] {
            set(role, color(c));
        }
        let font_family = match &state.font_family {
            Some(chosen) => chosen.as_str(),
            None if state.config.font_family.is_empty() => {
                state.appearance.as_ref().and_then(|a| a.font_family.as_deref()).unwrap_or_default()
            }
            None => state.config.font_family.as_str(),
        };
        set("font-family", string(font_family));
        set("font-size", Value::Number(state.config.font_size.into()));
        set("radius", Value::Number(state.config.radius.into()));
    });
}

/// Reads `<config dir>/theme.json`; called before every load so edits apply on reload.
pub fn load_config(host: &ShellHost) {
    let path = host.config().parent().map(|d| d.join("theme.json"));
    let config = match path.as_deref().map(ThemeConfig::load) {
        Some(Ok(config)) => config,
        Some(Err(e)) => {
            log::error!("{e}; using the default theme");
            ThemeConfig::default()
        }
        None => ThemeConfig::default(),
    };
    let mut names: Vec<String> = config.palettes.keys().cloned().collect();
    names.sort();
    names.extend(theme::presets().into_iter().map(|p| p.name));
    host.set_global("Theme", "palettes", list_value(names.iter().map(string)));
    STATE.with(|s| s.borrow_mut().config = config);
    publish(host);
}

pub fn install(host: &Rc<ShellHost>) {
    host.set_global(
        "Theme",
        "palettes",
        list_value(theme::presets().iter().map(|p| string(&p.name))),
    );
    publish(host);

    std::thread::Builder::new()
        .name("slintlu-fonts".into())
        .spawn(|| {
            let families = fonts::families();
            host::post(move |host| {
                host.set_global("Theme", "fonts", list_value(families.iter().map(string)));
                STATE.with(|s| s.borrow_mut().fonts = families);
            });
        })
        .map_err(|e| log::warn!("cannot list the installed fonts: {e}"))
        .ok();

    let weak = Rc::downgrade(host);
    host.on_callback(
        "Theme",
        "set-font-family",
        Rc::new(move |args| {
            let family = string_arg(args, 0).unwrap_or_default();
            let family = family.trim();
            STATE.with(|s| {
                s.borrow_mut().font_family = (!family.is_empty()).then(|| family.to_owned())
            });
            if let Some(host) = weak.upgrade() {
                publish(&host);
            }
            Value::Void
        }),
    );
    host.on_callback(
        "Theme",
        "search-fonts",
        Rc::new(|args| {
            let query = string_arg(args, 0).unwrap_or_default();
            let limit = number_arg(args, 1).filter(|l| *l > 0.0).map_or(usize::MAX, |l| l as usize);
            STATE.with(|s| {
                let s = s.borrow();
                list_value(fonts::matching(&s.fonts, &query, limit).into_iter().map(string))
            })
        }),
    );

    if let Ok(portal) = Portal::connect() {
        spawn_watcher("theme", move || {
            portal
                .watch(|appearance| {
                    let appearance = appearance.clone();
                    host::post(move |host| {
                        STATE.with(|s| s.borrow_mut().appearance = Some(appearance));
                        publish(host);
                    });
                })
                .map_err(std::io::Error::other)
        });
    }

    let weak = Rc::downgrade(host);
    host.on_callback(
        "Theme",
        "set-mode",
        Rc::new(move |args| {
            if let Some(mode) = string_arg(args, 0) {
                STATE.with(|s| s.borrow_mut().config.mode = mode);
                if let Some(host) = weak.upgrade() {
                    publish(&host);
                }
            }
            Value::Void
        }),
    );
    let weak = Rc::downgrade(host);
    host.on_callback(
        "Theme",
        "set-palette",
        Rc::new(move |args| {
            if let (Some(name), Some(host)) = (string_arg(args, 0), weak.upgrade()) {
                // The slot follows the mode, not whether the current palette is dark: after
                // choosing a light palette in dark mode, the next choice must replace it.
                STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    let prefers_dark = desktop_prefers_dark(&s);
                    theme::choose_palette(&mut s.config, name, prefers_dark);
                });
                publish(&host);
            }
            Value::Void
        }),
    );
}
