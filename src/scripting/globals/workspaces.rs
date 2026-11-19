//! The `Workspaces` global, backed by the compositor's IPC.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use slint_interpreter::Value;
use slintlu::compositors::{
    self, Action, Backend, Compositor, DesktopState, KeyBinding, SwitcherItem, SwitcherState,
    WallpaperEntry, Window,
};

use super::worker::{run_action, spawn_watcher};
use crate::scripting::bridge::{
    bool_arg, list_value, number_arg, string, string_arg, struct_value,
};
use crate::scripting::{ShellHost, host};

fn window(w: Option<&Window>) -> Value {
    let w = w.cloned().unwrap_or_default();
    struct_value([
        ("id", string(w.id.to_string())),
        ("title", string(&w.title)),
        ("app-id", string(&w.app_id)),
        ("workspace", Value::Number(w.workspace.unwrap_or(0) as f64)),
        ("focused", Value::Bool(w.focused)),
        ("urgent", Value::Bool(w.urgent)),
    ])
}

thread_local! {
    /// The compositor's preferences as last published (plus local changes not yet echoed).
    static PREFERENCES: RefCell<(serde_json::Value, u32)> =
        const { RefCell::new((serde_json::Value::Null, 0)) };
    /// The wallpaper library as last published, for `filter-wallpapers`.
    static WALLPAPERS: RefCell<Vec<WallpaperEntry>> = const { RefCell::new(Vec::new()) };
    /// The key bindings as last published: the model is only rebuilt when they change.
    static BINDINGS: RefCell<Vec<KeyBinding>> = const { RefCell::new(Vec::new()) };
}

/// Implements `Compositor.filter-wallpapers`.
fn filter_wallpapers(args: &[Value]) -> Value {
    let query = string_arg(args, 0).unwrap_or_default().to_lowercase();
    let collection = string_arg(args, 1).unwrap_or_default();
    let category = string_arg(args, 2).unwrap_or_default();
    let kind = string_arg(args, 3).unwrap_or_default();
    WALLPAPERS.with(|w| {
        list_value(
            w.borrow()
                .iter()
                .filter(|e| match collection.as_str() {
                    "bundled" => e.bundled,
                    "favorites" => e.favorite,
                    _ => true,
                })
                .filter(|e| category.is_empty() || e.category == category)
                .filter(|e| match kind.as_str() {
                    "image" => !e.video,
                    "video" => e.video,
                    _ => true,
                })
                .filter(|e| {
                    query.is_empty()
                        || e.name.to_lowercase().contains(&query)
                        || e.category.to_lowercase().contains(&query)
                })
                .map(wallpaper),
        )
    })
}

fn preference(key: &str) -> Option<serde_json::Value> {
    PREFERENCES.with(|p| p.borrow().0.get(key).cloned())
}

/// Stores new preferences and bumps the revision bindings pass to the readers.
fn set_preferences(host: &ShellHost, update: impl FnOnce(&mut serde_json::Value)) {
    let revision = PREFERENCES.with(|p| {
        let mut p = p.borrow_mut();
        update(&mut p.0);
        p.1 += 1;
        p.1
    });
    host.set_global("Compositor", "preferences-revision", Value::Number(revision.into()));
}

fn wallpaper(entry: &WallpaperEntry) -> Value {
    struct_value([
        ("path", string(&entry.path)),
        ("name", string(&entry.name)),
        ("category", string(&entry.category)),
        ("video", Value::Bool(entry.video)),
        ("bundled", Value::Bool(entry.bundled)),
        ("favorite", Value::Bool(entry.favorite)),
        ("preview", string(&entry.preview)),
    ])
}

fn publish(host: &ShellHost, state: &DesktopState) {
    let workspaces = state.workspaces.iter().map(|ws| {
        struct_value([
            ("id", Value::Number(ws.id as f64)),
            ("index", Value::Number(ws.index as f64)),
            ("name", string(&ws.name)),
            ("output", string(&ws.output)),
            ("active", Value::Bool(ws.active)),
            ("focused", Value::Bool(ws.focused)),
            ("urgent", Value::Bool(ws.urgent)),
            ("windows", Value::Number(ws.windows.into())),
        ])
    });
    host.set_global("Workspaces", "all", list_value(workspaces));
    host.set_global(
        "Workspaces",
        "windows",
        list_value(state.windows.iter().map(|w| window(Some(w)))),
    );
    host.set_global("Workspaces", "focused-window", window(state.focused_window()));
    host.set_global("Workspaces", "keyboard-layout", string(&state.keyboard_layout));
    let focused_output = state.workspaces.iter().find(|w| w.focused).map(|w| w.output.as_str());
    host.set_global("Workspaces", "focused-output", string(focused_output.unwrap_or_default()));
    if state.session_ending {
        log::info!("the compositor is ending the session: quitting");
        let _ = slint::quit_event_loop();
        return;
    }
    host.set_global("Compositor", "wallpaper", string(&state.wallpaper));
    host.set_global("Compositor", "wallpaper-poster", string(&state.wallpaper_poster));
    host.set_global("Compositor", "wallpaper-live", Value::Bool(state.wallpaper_live));
    if WALLPAPERS.with(|w| *w.borrow() != state.wallpapers) {
        let mut categories: Vec<&str> = state
            .wallpapers
            .iter()
            .map(|e| e.category.as_str())
            .filter(|c| !c.is_empty())
            .collect();
        categories.sort_unstable();
        categories.dedup();
        host.set_global(
            "Compositor",
            "wallpaper-categories",
            list_value(categories.into_iter().map(string)),
        );
        WALLPAPERS.with(|w| *w.borrow_mut() = state.wallpapers.clone());
        host.set_global(
            "Compositor",
            "wallpapers",
            list_value(state.wallpapers.iter().map(wallpaper)),
        );
    }
    host.set_global("Compositor", "wallpaper-directory", string(&state.wallpaper_directory));
    let preferences = serde_json::from_str(&state.preferences).unwrap_or(serde_json::Value::Null);
    if PREFERENCES.with(|p| p.borrow().0 != preferences) {
        set_preferences(host, |p| *p = preferences);
    }
    host.set_global("Compositor", "language", string(&state.language));
    if BINDINGS.with(|b| *b.borrow() != state.bindings) {
        let bindings = state.bindings.iter().map(|binding| {
            let (name, argument) =
                binding.action.split_once(' ').unwrap_or((binding.action.as_str(), ""));
            struct_value([
                ("mode", string(&binding.mode)),
                ("keys", list_value(binding.keys.iter().map(string))),
                ("combo", string(binding.keys.join("+"))),
                ("action", string(&binding.action)),
                ("name", string(name)),
                ("argument", string(argument.trim())),
            ])
        });
        host.set_global("Compositor", "bindings", list_value(bindings));
        BINDINGS.with(|b| *b.borrow_mut() = state.bindings.clone());
    }
}

fn action(
    host: &ShellHost,
    backend: &Arc<dyn Backend>,
    callback: &str,
    make: fn(&[Value]) -> Option<Action>,
) {
    let backend = backend.clone();
    host.on_callback(
        "Workspaces",
        callback,
        Rc::new(move |args| {
            if let Some(action) = make(args) {
                let backend = backend.clone();
                run_action("workspaces", move || backend.perform(&action));
            }
            Value::Void
        }),
    );
}

fn switcher_entry(item: &SwitcherItem) -> Value {
    let icon = |app_id: &str| {
        Value::Image(
            super::apps::icon_for(app_id)
                .filter(|i| i.size().width > 0)
                .unwrap_or_else(|| super::apps::theme_icon(app_id)),
        )
    };
    struct_value([
        ("id", Value::Number(item.id as f64)),
        ("title", string(&item.title)),
        ("app-id", string(&item.app_id)),
        ("icon", icon(&item.app_id)),
        ("workspace", Value::Number(item.workspace as f64)),
        ("focused", Value::Bool(item.focused)),
        ("minimized", Value::Bool(item.minimized)),
        ("icons", list_value(item.apps.iter().map(|a| icon(a)))),
        ("preview", Value::Image(preview(&item.preview))),
    ])
}

thread_local! {
    // Window pictures by file: each switch writes new files, so a path is never reused.
    static PREVIEWS: RefCell<std::collections::HashMap<String, slint::Image>> = RefCell::default();
    // The entries last shown: moving the selection must not rebuild the tiles.
    static SHOWN: RefCell<(bool, Vec<SwitcherItem>)> = RefCell::default();
}

fn preview(path: &str) -> slint::Image {
    if path.is_empty() {
        return slint::Image::default();
    }
    PREVIEWS.with(|cache| {
        cache
            .borrow_mut()
            .entry(path.to_owned())
            .or_insert_with(|| slint::Image::load_from_path(path.as_ref()).unwrap_or_default())
            .clone()
    })
}

fn publish_switcher(host: &ShellHost, state: &SwitcherState) {
    let changed = SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        let changed = shown.0 != state.workspaces || shown.1 != state.items;
        if changed {
            *shown = (state.workspaces, state.items.clone());
        }
        changed
    });
    if changed {
        host.set_global("Switcher", "entries", list_value(state.items.iter().map(switcher_entry)));
    }
    if !state.active {
        // The next switch writes new pictures.
        PREVIEWS.with(|cache| cache.borrow_mut().clear());
    }
    host.set_global("Switcher", "workspaces", Value::Bool(state.workspaces));
    host.set_global("Switcher", "index", Value::Number(state.index as f64));
    host.set_global("Switcher", "active", Value::Bool(state.active));
}

fn switcher_action(
    host: &ShellHost,
    backend: &Arc<dyn Backend>,
    callback: &str,
    make: fn(&[Value]) -> Option<Action>,
) {
    let backend = backend.clone();
    host.on_callback(
        "Switcher",
        callback,
        Rc::new(move |args| {
            if let Some(action) = make(args) {
                let backend = backend.clone();
                run_action("switcher", move || backend.perform(&action));
            }
            Value::Void
        }),
    );
}

/// The preference readers of the `Compositor` global: fallbacks without a compositor.
fn install_preference_readers(host: &Rc<ShellHost>) {
    host.on_callback(
        "Compositor",
        "preference-number",
        Rc::new(|args| {
            let fallback = number_arg(args, 1).unwrap_or(0.0);
            let key = string_arg(args, 0).unwrap_or_default();
            Value::Number(preference(&key).and_then(|v| v.as_f64()).unwrap_or(fallback))
        }),
    );
    host.on_callback(
        "Compositor",
        "preference-bool",
        Rc::new(|args| {
            let fallback = bool_arg(args, 1).unwrap_or(false);
            let key = string_arg(args, 0).unwrap_or_default();
            Value::Bool(preference(&key).and_then(|v| v.as_bool()).unwrap_or(fallback))
        }),
    );
    host.on_callback(
        "Compositor",
        "preference-string",
        Rc::new(|args| {
            let fallback = string_arg(args, 1).unwrap_or_default();
            let key = string_arg(args, 0).unwrap_or_default();
            string(preference(&key).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or(fallback))
        }),
    );
}

/// Turns a setter's arguments into the JSON value it stores.
type PreferenceConverter = fn(&[Value]) -> Option<serde_json::Value>;

/// The preference writers, sending `appearance` commands.
fn install_preference_writers(host: &Rc<ShellHost>, backend: &Arc<dyn Backend>) {
    let setters: [(&str, PreferenceConverter); 3] = [
        ("set-preference-number", |a| {
            // Whole numbers stay integers: LunaDash validates most numeric keys as integers.
            let n = number_arg(a, 1)?;
            Some(if n.fract() == 0.0 { serde_json::json!(n as i64) } else { serde_json::json!(n) })
        }),
        ("set-preference-bool", |a| bool_arg(a, 1).map(serde_json::Value::Bool)),
        ("set-preference-string", |a| string_arg(a, 1).map(serde_json::Value::String)),
    ];
    for (name, convert) in setters {
        let backend = backend.clone();
        let weak = Rc::downgrade(host);
        host.on_callback(
            "Compositor",
            name,
            Rc::new(move |args| {
                let (Some(key), Some(value), Some(host)) =
                    (string_arg(args, 0), convert(args), weak.upgrade())
                else {
                    return Value::Void;
                };
                let changes = serde_json::json!({ key.clone(): value.clone() }).to_string();
                set_preferences(&host, |p| {
                    if !p.is_object() {
                        *p = serde_json::json!({});
                    }
                    p[key.as_str()] = value;
                });
                let backend = backend.clone();
                run_action("compositor", move || backend.command("appearance", &changes).map(drop));
                Value::Void
            }),
        );
    }
}

fn window_id(args: &[Value]) -> Option<u64> {
    string_arg(args, 0)?.parse().ok()
}

/// The action bound to `keys` ("Meta+Shift+S") in `mode`, compared without regard to case.
fn binding_for(bindings: &[KeyBinding], mode: &str, keys: &str) -> Option<String> {
    bindings
        .iter()
        .find(|b| b.mode == mode && b.keys.join("+").eq_ignore_ascii_case(keys.trim()))
        .map(|b| b.action.clone())
}

pub fn install(host: &Rc<ShellHost>, compositor: Compositor) {
    host.set_global("Compositor", "name", string(compositor.as_str()));
    host.set_global("Compositor", "has-commands", Value::Bool(compositor == Compositor::LunaDash));
    host.on_callback(
        "Compositor",
        "binding-for",
        Rc::new(|args| {
            let mode = string_arg(args, 0).unwrap_or_default();
            let keys = string_arg(args, 1).unwrap_or_default();
            string(
                BINDINGS.with(|bindings| {
                    binding_for(&bindings.borrow(), &mode, &keys).unwrap_or_default()
                }),
            )
        }),
    );
    host.on_callback(
        "Compositor",
        "keys-for",
        Rc::new(|args| {
            let mode = string_arg(args, 0).unwrap_or_default();
            let action = string_arg(args, 1).unwrap_or_default();
            BINDINGS.with(|bindings| {
                let bindings = bindings.borrow();
                // A combination with modifiers before a lone key (Meta+Shift+F before F12).
                let of_action = || bindings.iter().filter(|b| b.mode == mode && b.action == action);
                let keys = of_action().find(|b| b.keys.len() > 1).or_else(|| of_action().next());
                list_value(keys.into_iter().flat_map(|b| b.keys.iter().map(string)))
            })
        }),
    );
    install_preference_readers(host);
    host.on_callback("Compositor", "filter-wallpapers", Rc::new(filter_wallpapers));
    let Some(backend) = compositors::backend(compositor) else {
        log::info!("workspaces: no IPC support for the {compositor} compositor");
        host.set_global("Workspaces", "available", Value::Bool(false));
        return;
    };
    host.set_global("Workspaces", "available", Value::Bool(true));

    let watcher = backend.clone();
    spawn_watcher("workspaces", move || {
        watcher.watch(&mut |state| {
            let state = state.clone();
            host::post(move |host| publish(host, &state));
        })
    });

    // One thread sends the commands, in the order the shell gave them: a
    // thread each could deliver "stop capturing keys" before "start".
    let (send, queue) = std::sync::mpsc::channel::<(String, String)>();
    let commands = backend.clone();
    let spawned = std::thread::Builder::new().name("slintlu-commands".into()).spawn(move || {
        for (method, value) in queue {
            // The shell sees why the last command failed ("" once one succeeds).
            let error = match commands.command(&method, &value) {
                Ok(_) => String::new(),
                Err(e) => {
                    log::warn!("compositor command {method}: {e}");
                    e.to_string().trim_start_matches("LunaDash: ").to_owned()
                }
            };
            host::post(move |host| host.set_global("Compositor", "command-error", string(error)));
        }
    });
    if let Err(e) = spawned {
        log::error!("cannot start the compositor command thread: {e}");
    }
    let commands = send.clone();
    host.on_callback(
        "Compositor",
        "command",
        Rc::new(move |args| {
            if let Some(method) = string_arg(args, 0) {
                let value = string_arg(args, 1).unwrap_or_default();
                let _ = commands.send((method, value));
            }
            Value::Void
        }),
    );
    // Bindings as JSON written here: an action may hold quotes ("spawn kitty --class \"a b\"").
    let commands = send.clone();
    host.on_callback(
        "Compositor",
        "bind",
        Rc::new(move |args| {
            let text = |index| string_arg(args, index).unwrap_or_default();
            let value = serde_json::json!({
                "mode": text(0), "keys": text(1), "action": text(2), "replace": text(3)
            });
            let _ = commands.send(("bind".into(), value.to_string()));
            Value::Void
        }),
    );
    host.on_callback(
        "Compositor",
        "unbind",
        Rc::new(move |args| {
            let text = |index| string_arg(args, index).unwrap_or_default();
            let value = serde_json::json!({ "mode": text(0), "keys": text(1) });
            let _ = send.send(("unbind".into(), value.to_string()));
            Value::Void
        }),
    );
    if compositor == Compositor::LunaDash {
        host.set_global("Switcher", "available", Value::Bool(true));
        let switcher = backend.clone();
        spawn_watcher("switcher", move || {
            switcher.watch_switcher(&mut |state| {
                let state = state.clone();
                host::post(move |host| publish_switcher(host, &state));
            })
        });
        switcher_action(host, &backend, "open", |a| {
            Some(Action::OpenSwitcher { workspaces: matches!(a.first(), Some(Value::Bool(true))) })
        });
        switcher_action(host, &backend, "step", |a| {
            Some(Action::StepSwitcher(number_arg(a, 0)? as i32))
        });
        switcher_action(host, &backend, "choose", |a| {
            Some(Action::ChooseSwitcher(number_arg(a, 0)? as u64))
        });
        switcher_action(host, &backend, "cancel", |_| {
            Some(Action::FinishSwitcher { accept: false })
        });
    }
    install_preference_writers(host, &backend);
    action(host, &backend, "focus", |a| Some(Action::FocusWorkspace(number_arg(a, 0)? as u64)));
    action(host, &backend, "focus-window", |a| Some(Action::FocusWindow(window_id(a)?)));
    action(host, &backend, "close-window", |a| Some(Action::CloseWindow(window_id(a)?)));
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    #[test]
    fn finds_the_action_of_keys_in_a_mode() {
        let binding = |mode: &str, keys: &[&str], action: &str| KeyBinding {
            mode: mode.into(),
            keys: keys.iter().map(|k| k.to_string()).collect(),
            action: action.into(),
        };
        let bindings = vec![
            binding("normal", &["Meta", "Q"], "close-window"),
            binding("normal", &["Meta", "Shift", "S"], "screenshot"),
            binding("overview", &["Escape"], "close-overview"),
        ];
        assert_eq!(binding_for(&bindings, "normal", "meta+q").as_deref(), Some("close-window"));
        assert_eq!(binding_for(&bindings, "normal", "Meta+Shift+S").as_deref(), Some("screenshot"));
        assert_eq!(binding_for(&bindings, "normal", "Escape"), None, "another mode");
        assert_eq!(binding_for(&bindings, "overview", "Escape").as_deref(), Some("close-overview"));
    }
}
