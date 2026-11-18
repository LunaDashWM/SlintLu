//! The `Apps` global: installed applications, search and launching.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint_interpreter::Value;
use slintlu::launcher::{self, AppEntry, LaunchHistory};

use super::icons::{self, DEFAULT_SIZE};
use crate::config::Watcher;
use crate::scripting::bridge::{list_value, number_arg, string, string_arg, struct_value};
use crate::scripting::{ShellHost, host};

/// A scanned application with its icon file resolved off the UI thread.
struct ScannedApp {
    entry: AppEntry,
    icon: Option<PathBuf>,
}

struct AppsState {
    apps: Vec<AppEntry>,
    values: Vec<Value>,
    history: LaunchHistory,
}

thread_local! {
    static STATE: RefCell<Option<Rc<RefCell<AppsState>>>> = const { RefCell::new(None) };
    static WATCHER: RefCell<Option<Watcher>> = const { RefCell::new(None) };
}

fn state() -> Option<Rc<RefCell<AppsState>>> {
    STATE.with(|s| s.borrow().clone())
}

fn value(app: &ScannedApp) -> Value {
    let e = &app.entry;
    struct_value([
        ("id", string(&e.id)),
        ("name", string(&e.name)),
        ("generic-name", string(&e.generic_name)),
        ("comment", string(&e.comment)),
        ("icon", Value::Image(app.icon.as_deref().map(icons::image_at).unwrap_or_default())),
        ("icon-name", string(&e.icon)),
        ("terminal", Value::Bool(e.terminal)),
        ("categories", list_value(e.categories.iter().map(string))),
        (
            "actions",
            list_value(
                e.actions
                    .iter()
                    .map(|a| struct_value([("id", string(&a.id)), ("name", string(&a.name))])),
            ),
        ),
    ])
}

/// Scans in the background and publishes the result on the event loop.
fn rescan() {
    std::thread::spawn(|| {
        let language = slintlu::i18n::current_language();
        let lookup = icons::lookup();
        let apps: Vec<ScannedApp> =
            launcher::scan(&launcher::application_dirs(), language.as_deref())
                .into_iter()
                .map(|entry| ScannedApp { icon: lookup.lookup(&entry.icon, DEFAULT_SIZE), entry })
                .collect();
        host::post(move |host| {
            let Some(state) = state() else { return };
            let values: Vec<Value> = apps.iter().map(value).collect();
            host.set_global("Apps", "all", list_value(values.iter().cloned()));
            let mut state = state.borrow_mut();
            state.apps = apps.into_iter().map(|a| a.entry).collect();
            state.values = values;
            log::debug!("{} applications", state.apps.len());
        });
    });
}

/// The index of the application a window app id belongs to.
fn index_for_app_id(apps: &[AppEntry], app_id: &str) -> Option<usize> {
    let app_id = app_id.trim_end_matches(".desktop");
    if app_id.is_empty() {
        return None;
    }
    let exact = apps.iter().position(|a| {
        a.id.trim_end_matches(".desktop").eq_ignore_ascii_case(app_id)
            || a.startup_wm_class.eq_ignore_ascii_case(app_id)
    });
    // "org.gnome.Nautilus" windows of "nautilus.desktop" and the like.
    let last = |s: &str| s.rsplit('.').next().unwrap_or(s).to_ascii_lowercase();
    exact.or_else(|| {
        apps.iter().position(|a| last(a.id.trim_end_matches(".desktop")) == last(app_id))
    })
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match value {
        Value::Struct(s) => s.get_field(name),
        _ => None,
    }
}

fn text_field(value: &Value, name: &str) -> String {
    match field(value, name) {
        Some(Value::String(s)) => s.to_string(),
        _ => String::new(),
    }
}

fn bool_field(value: &Value, name: &str) -> bool {
    matches!(field(value, name), Some(Value::Bool(true)))
}

fn strings(value: Option<&Value>) -> Vec<Value> {
    use slint::Model;
    match value {
        Some(Value::Model(model)) => model.iter().collect(),
        _ => Vec::new(),
    }
}

#[derive(Default)]
struct Group {
    app: Option<Value>,
    app_id: String,
    pinned: bool,
    windows: Vec<String>,
    focused: bool,
    urgent: bool,
}

/// Implements `Apps.group-windows`.
fn group_windows(args: &[Value]) -> Value {
    let Some(state) = state() else { return list_value([]) };
    let state = state.borrow();
    let mut groups: Vec<(String, Group)> = Vec::new();
    for id in strings(args.first()) {
        let Value::String(id) = id else { continue };
        let index = state
            .apps
            .iter()
            .position(|a| a.id == id.as_str() || a.id.trim_end_matches(".desktop") == id.as_str());
        let key = index.map_or_else(|| id.to_string(), |i| state.apps[i].id.clone());
        if groups.iter().any(|(k, _)| *k == key) {
            continue;
        }
        let app = index.and_then(|i| state.values.get(i).cloned());
        groups.push((key, Group { app, pinned: true, ..Group::default() }));
    }
    for window in strings(args.get(1)) {
        let app_id = text_field(&window, "app-id");
        let index = index_for_app_id(&state.apps, &app_id);
        let key = index.map_or_else(|| format!("window:{app_id}"), |i| state.apps[i].id.clone());
        let position = match groups.iter().position(|(k, _)| *k == key) {
            Some(position) => position,
            None => {
                let app = index.and_then(|i| state.values.get(i).cloned());
                groups.push((key, Group { app, ..Group::default() }));
                groups.len() - 1
            }
        };
        let group = &mut groups[position].1;
        if group.app_id.is_empty() {
            group.app_id = app_id;
        }
        let id = text_field(&window, "id");
        if bool_field(&window, "focused") {
            group.focused = true;
            group.windows.insert(0, id);
        } else {
            group.windows.push(id);
        }
        group.urgent |= bool_field(&window, "urgent");
    }
    list_value(groups.into_iter().map(|(key, g)| {
        let app = g.app.unwrap_or_else(|| {
            let name = if g.app_id.is_empty() { key.clone() } else { g.app_id.clone() };
            let icon = icons::icon(&name.to_lowercase(), DEFAULT_SIZE);
            // A pinned application keeps its desktop file id, so launching works as soon as
            // the scan finds it; windows without a desktop file have none.
            let id = if g.pinned { key.clone() } else { String::new() };
            let mut value = value(&ScannedApp {
                entry: AppEntry { id, name, ..Default::default() },
                icon: None,
            });
            if let Value::Struct(s) = &mut value {
                s.set_field("icon".into(), Value::Image(icon));
            }
            value
        });
        struct_value([
            ("app", app),
            ("app-id", string(&g.app_id)),
            ("pinned", Value::Bool(g.pinned)),
            ("windows", list_value(g.windows.iter().map(string))),
            ("focused", Value::Bool(g.focused)),
            ("urgent", Value::Bool(g.urgent)),
        ])
    }))
}

/// The icon of the application with this desktop file id (or `StartupWMClass` / app id).
pub fn icon_for(app_id: &str) -> Option<slint::Image> {
    let state = state()?;
    let state = state.borrow();
    let index = index_for_app_id(&state.apps, app_id)?;
    match &state.values[index] {
        Value::Struct(s) => match s.get_field("icon") {
            Some(Value::Image(image)) => Some(image.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn launch(id: &str, action: Option<&str>) {
    let Some(state) = state() else { return };
    let mut state = state.borrow_mut();
    let Some(app) = state.apps.iter().find(|a| a.id == id).cloned() else {
        return log::warn!("no application with id {id}");
    };
    match launcher::launch(&app, action) {
        Ok(()) => state.history.record(id),
        Err(e) => log::error!("{}: {e}", app.name),
    }
}

/// Releases cached values while Slint is still alive.
pub fn shutdown() {
    STATE.with(|s| s.borrow_mut().take());
    WATCHER.with(|w| w.borrow_mut().take());
}

pub fn install(host: &Rc<ShellHost>) {
    let initial = AppsState {
        apps: Vec::new(),
        values: Vec::new(),
        history: LaunchHistory::load(LaunchHistory::default_path()),
    };
    STATE.with(|s| *s.borrow_mut() = Some(Rc::new(RefCell::new(initial))));
    rescan();

    let dirs: Vec<PathBuf> =
        launcher::application_dirs().into_iter().filter(|d| d.is_dir()).collect();
    match Watcher::new(&dirs, rescan) {
        Ok(watcher) => WATCHER.with(|w| *w.borrow_mut() = Some(watcher)),
        Err(e) => log::warn!("applications will not refresh automatically: {e}"),
    }

    host.on_callback(
        "Apps",
        "search",
        Rc::new(|args| {
            let Some(state) = state() else { return list_value([]) };
            let state = state.borrow();
            let query = string_arg(args, 0).unwrap_or_default();
            let limit = number_arg(args, 1).filter(|l| *l > 0.0).map_or(usize::MAX, |l| l as usize);
            let results = launcher::search(&state.apps, &query, &state.history, limit);
            list_value(results.into_iter().filter_map(|app| {
                let index = state.apps.iter().position(|a| a.id == app.id)?;
                state.values.get(index).cloned()
            }))
        }),
    );
    host.on_callback(
        "Apps",
        "search-in",
        Rc::new(|args| {
            let Some(state) = state() else { return list_value([]) };
            let state = state.borrow();
            let query = string_arg(args, 0).unwrap_or_default();
            let categories = string_arg(args, 1).unwrap_or_default();
            let limit = number_arg(args, 2).filter(|l| *l > 0.0).map_or(usize::MAX, |l| l as usize);
            let results = launcher::search(&state.apps, &query, &state.history, usize::MAX);
            list_value(
                results
                    .into_iter()
                    .filter(|app| launcher::in_categories(app, &categories))
                    .take(limit)
                    .filter_map(|app| {
                        let index = state.apps.iter().position(|a| a.id == app.id)?;
                        state.values.get(index).cloned()
                    }),
            )
        }),
    );
    host.on_callback(
        "Apps",
        "get",
        Rc::new(|args| {
            let id = string_arg(args, 0).unwrap_or_default();
            let found = state().and_then(|state| {
                let state = state.borrow();
                let index = state
                    .apps
                    .iter()
                    .position(|a| a.id == id || a.id.trim_end_matches(".desktop") == id)?;
                state.values.get(index).cloned()
            });
            found.unwrap_or_else(|| {
                value(&ScannedApp { entry: AppEntry { id, ..Default::default() }, icon: None })
            })
        }),
    );
    host.on_callback("Apps", "group-windows", Rc::new(group_windows));
    host.on_callback(
        "Apps",
        "icon-for",
        Rc::new(|args| {
            let app_id = string_arg(args, 0).unwrap_or_default();
            let image = icon_for(&app_id)
                .filter(|i| i.size().width > 0)
                .unwrap_or_else(|| icons::icon(&app_id.to_lowercase(), DEFAULT_SIZE));
            Value::Image(image)
        }),
    );
    host.on_callback(
        "Apps",
        "launch",
        Rc::new(|args| {
            if let Some(id) = string_arg(args, 0) {
                launch(&id, None);
            }
            Value::Void
        }),
    );
    host.on_callback(
        "Apps",
        "launch-action",
        Rc::new(|args| {
            if let (Some(id), Some(action)) = (string_arg(args, 0), string_arg(args, 1)) {
                launch(&id, Some(&action));
            }
            Value::Void
        }),
    );
}
