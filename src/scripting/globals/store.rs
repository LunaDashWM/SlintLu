//! The `Store` (persistent settings) and `Shared` (cross-window state) globals.

use std::cell::RefCell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use serde_json::{Value as Json, json};
use slint_interpreter::Value;
use slintlu::store::Store;
use slintlu::util::xdg;

use crate::scripting::ShellHost;
use crate::scripting::bridge::{bool_arg, list_value, number_arg, string, string_arg};

/// One key–value global: its Slint name, the values and a change counter.
struct KeyValues {
    global: &'static str,
    store: RefCell<Store>,
    revision: RefCell<u32>,
}

impl KeyValues {
    fn new(global: &'static str) -> Rc<Self> {
        Rc::new(Self { global, store: RefCell::default(), revision: RefCell::new(0) })
    }

    fn bump(&self, host: &ShellHost) {
        let revision = {
            let mut r = self.revision.borrow_mut();
            *r += 1;
            *r
        };
        host.set_global(self.global, "revision", Value::Number(revision.into()));
    }

    fn set(self: &Rc<Self>, host: &ShellHost, key: &str, value: Json) {
        self.store.borrow_mut().set(key, value);
        self.bump(host);
    }
}

fn getter(
    host: &Rc<ShellHost>,
    kv: &Rc<KeyValues>,
    name: &str,
    read: fn(&Store, &[Value]) -> Value,
) {
    let kv2 = kv.clone();
    host.on_callback(kv.global, name, Rc::new(move |args| read(&kv2.store.borrow(), args)));
}

fn setter(
    host: &Rc<ShellHost>,
    kv: &Rc<KeyValues>,
    name: &str,
    convert: fn(&[Value]) -> Option<Json>,
) {
    let weak = Rc::downgrade(host);
    let kv2 = kv.clone();
    host.on_callback(
        kv.global,
        name,
        Rc::new(move |args| {
            if let (Some(key), Some(value), Some(host)) =
                (string_arg(args, 0), convert(args), weak.upgrade())
            {
                kv2.set(&host, &key, value);
            }
            Value::Void
        }),
    );
}

fn key(args: &[Value]) -> String {
    string_arg(args, 0).unwrap_or_default()
}

fn install_key_values(host: &Rc<ShellHost>, kv: &Rc<KeyValues>) {
    getter(host, kv, "get-string", |store, args| {
        let fallback = string_arg(args, 1).unwrap_or_default();
        string(store.get(&key(args)).and_then(Json::as_str).map_or(fallback, str::to_owned))
    });
    getter(host, kv, "get-number", |store, args| {
        let fallback = number_arg(args, 1).unwrap_or(0.0);
        Value::Number(store.get(&key(args)).and_then(Json::as_f64).unwrap_or(fallback))
    });
    getter(host, kv, "get-bool", |store, args| {
        let fallback = bool_arg(args, 1).unwrap_or(false);
        Value::Bool(store.get(&key(args)).and_then(Json::as_bool).unwrap_or(fallback))
    });
    setter(host, kv, "set-string", |a| string_arg(a, 1).map(Json::String));
    setter(host, kv, "set-number", |a| number_arg(a, 1).map(|n| json!(n)));
    setter(host, kv, "set-bool", |a| bool_arg(a, 1).map(Json::Bool));

    let weak = Rc::downgrade(host);
    let kv2 = kv.clone();
    host.on_callback(
        kv.global,
        "remove",
        Rc::new(move |args| {
            if let Some(host) = weak.upgrade() {
                kv2.store.borrow_mut().remove(&key(args));
                kv2.bump(&host);
            }
            Value::Void
        }),
    );
    let weak = Rc::downgrade(host);
    let kv2 = kv.clone();
    host.on_callback(
        kv.global,
        "toggle",
        Rc::new(move |args| {
            if let Some(host) = weak.upgrade() {
                let key = key(args);
                let current = kv2.store.borrow().get(&key).and_then(Json::as_bool).unwrap_or(false);
                kv2.set(&host, &key, Json::Bool(!current));
            }
            Value::Void
        }),
    );
}

/// A `[string]` callback argument.
fn strings(args: &[Value], index: usize) -> Option<Vec<String>> {
    use slint::Model;
    match args.get(index) {
        Some(Value::Model(model)) => Some(
            model
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

fn list(store: &Store, key: &str) -> Vec<String> {
    store
        .get(key)
        .and_then(Json::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// A list position from a callback argument, when it names an item of `items`.
fn position(items: &[String], index: Option<f64>) -> Option<usize> {
    let index = index?;
    (index >= 0.0 && index.fract() == 0.0 && index < items.len() as f64).then_some(index as usize)
}

/// Inserts `value` before position `index`; past the end or negative appends.
fn insert_at(items: &mut Vec<String>, index: Option<f64>, value: String) {
    let at = position(items, index).unwrap_or(items.len());
    items.insert(at, value);
}

/// Moves the item at `from` to position `to` (clamped to the list).
fn move_item(items: &mut Vec<String>, from: Option<f64>, to: Option<f64>) {
    let (Some(from), Some(to)) = (position(items, from), to) else { return };
    let last = items.len() - 1;
    let to = if to.is_nan() { from } else { to.clamp(0.0, last as f64) as usize };
    let item = items.remove(from);
    items.insert(to, item);
}

fn remove_at(items: &mut Vec<String>, index: Option<f64>) {
    if let Some(index) = position(items, index) {
        items.remove(index);
    }
}

/// A callback that edits the list named by its first argument and stores the result.
fn list_editor(
    host: &Rc<ShellHost>,
    kv: &Rc<KeyValues>,
    name: &str,
    edit: fn(&mut Vec<String>, &[Value]),
) {
    let weak = Rc::downgrade(host);
    let kv2 = kv.clone();
    host.on_callback(
        kv.global,
        name,
        Rc::new(move |args| {
            if let Some(host) = weak.upgrade() {
                let key = key(args);
                let mut items = list(&kv2.store.borrow(), &key);
                edit(&mut items, args);
                kv2.set(&host, &key, Json::Array(items.into_iter().map(Json::String).collect()));
            }
            Value::Void
        }),
    );
}

fn install_lists(host: &Rc<ShellHost>, kv: &Rc<KeyValues>) {
    getter(host, kv, "get-list", |store, args| {
        list_value(list(store, &key(args)).into_iter().map(string))
    });
    getter(host, kv, "list-contains", |store, args| {
        let value = string_arg(args, 1).unwrap_or_default();
        Value::Bool(list(store, &key(args)).contains(&value))
    });
    setter(host, kv, "set-list", |a| {
        strings(a, 1).map(|s| Json::Array(s.into_iter().map(Json::String).collect()))
    });
    for (name, add) in [("list-add", true), ("list-remove", false)] {
        let weak = Rc::downgrade(host);
        let kv2 = kv.clone();
        host.on_callback(
            kv.global,
            name,
            Rc::new(move |args| {
                if let (Some(value), Some(host)) = (string_arg(args, 1), weak.upgrade()) {
                    let key = key(args);
                    let mut items = list(&kv2.store.borrow(), &key);
                    items.retain(|v| *v != value);
                    if add {
                        items.push(value);
                    }
                    kv2.set(
                        &host,
                        &key,
                        Json::Array(items.into_iter().map(Json::String).collect()),
                    );
                }
                Value::Void
            }),
        );
    }
}

fn install_list_edits(host: &Rc<ShellHost>, kv: &Rc<KeyValues>) {
    list_editor(host, kv, "list-insert", |items, args| {
        if let Some(value) = string_arg(args, 2) {
            insert_at(items, number_arg(args, 1), value);
        }
    });
    list_editor(host, kv, "list-move", |items, args| {
        move_item(items, number_arg(args, 1), number_arg(args, 2));
    });
    list_editor(host, kv, "list-remove-at", |items, args| remove_at(items, number_arg(args, 1)));
}

/// Named copies of the Store's values (profiles), kept in a file of their own.
fn install_profiles(host: &Rc<ShellHost>, kv: &Rc<KeyValues>, profiles: Store) {
    let profiles = Rc::new(RefCell::new(profiles));
    let p = profiles.clone();
    host.on_callback(
        kv.global,
        "profiles",
        Rc::new(move |_| list_value(p.borrow().keys().map(string))),
    );
    let p = profiles.clone();
    host.on_callback(
        kv.global,
        "profile-string",
        Rc::new(move |args| {
            let fallback = string_arg(args, 2).unwrap_or_default();
            let value = p
                .borrow()
                .get(&key(args))
                .and_then(|profile| profile.get(string_arg(args, 1).unwrap_or_default()))
                .and_then(Json::as_str)
                .map(str::to_owned);
            string(value.unwrap_or(fallback))
        }),
    );
    let action = |run: fn(&KeyValues, &RefCell<Store>, String, Vec<String>)| {
        let weak = Rc::downgrade(host);
        let (kv, profiles) = (kv.clone(), profiles.clone());
        Rc::new(move |args: &[Value]| {
            let name = key(args);
            if let (false, Some(host)) = (name.is_empty(), weak.upgrade()) {
                run(&kv, &profiles, name, strings(args, 1).unwrap_or_default());
                kv.bump(&host);
            }
            Value::Void
        })
    };
    host.on_callback(
        kv.global,
        "save-profile",
        action(|kv, profiles, name, prefixes| {
            let values = kv.store.borrow().matching(&prefixes);
            profiles.borrow_mut().set(&name, Json::Object(values.into_iter().collect()));
        }),
    );
    host.on_callback(
        kv.global,
        "load-profile",
        action(|kv, profiles, name, prefixes| {
            if let Some(Json::Object(values)) = profiles.borrow().get(&name) {
                let values = values.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                kv.store.borrow_mut().replace_matching(&prefixes, values);
            }
        }),
    );
    host.on_callback(
        kv.global,
        "delete-profile",
        action(|_, profiles, name, _| profiles.borrow_mut().remove(&name)),
    );
}

/// Copies values between the Store and Shared, e.g. to edit a draft in Shared and keep it
/// only when the user confirms.
fn install_copies(host: &Rc<ShellHost>, persistent: &Rc<KeyValues>, shared: &Rc<KeyValues>) {
    for (name, from, to) in [
        ("copy-from-store", persistent.clone(), shared.clone()),
        ("copy-to-store", shared.clone(), persistent.clone()),
    ] {
        let weak = Rc::downgrade(host);
        host.on_callback(
            "Shared",
            name,
            Rc::new(move |args| {
                if let (Some(prefixes), Some(host)) = (strings(args, 0), weak.upgrade()) {
                    let values = from.store.borrow().matching(&prefixes);
                    to.store.borrow_mut().replace_matching(&prefixes, values);
                    to.bump(&host);
                }
                Value::Void
            }),
        );
    }
}

pub fn install(host: &Rc<ShellHost>) {
    let mut hasher = DefaultHasher::new();
    host.config().hash(&mut hasher);
    let hash = hasher.finish();
    let directory = xdg::state_home().join("slintlu");
    let path = directory.join(format!("store-{hash:016x}.json"));
    log::debug!("store: {}", path.display());
    let persistent = KeyValues::new("Store");
    *persistent.store.borrow_mut() = Store::load(&path);
    install_key_values(host, &persistent);
    install_lists(host, &persistent);
    install_list_edits(host, &persistent);
    install_profiles(
        host,
        &persistent,
        Store::load(&directory.join(format!("profiles-{hash:016x}.json"))),
    );
    let shared = KeyValues::new("Shared");
    install_key_values(host, &shared);
    install_lists(host, &shared);
    install_list_edits(host, &shared);
    install_copies(host, &persistent, &shared);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn inserts_at_a_position_or_appends() {
        let mut list = items(&["a", "c"]);
        insert_at(&mut list, Some(1.0), "b".into());
        insert_at(&mut list, Some(9.0), "d".into());
        insert_at(&mut list, Some(-1.0), "d".into());
        insert_at(&mut list, None, "e".into());
        assert_eq!(list, items(&["a", "b", "c", "d", "d", "e"]));
    }

    #[test]
    fn moves_within_the_list() {
        let mut list = items(&["a", "b", "c"]);
        move_item(&mut list, Some(2.0), Some(0.0));
        assert_eq!(list, items(&["c", "a", "b"]));
        move_item(&mut list, Some(0.0), Some(7.0));
        assert_eq!(list, items(&["a", "b", "c"]));
        move_item(&mut list, Some(1.0), Some(-3.0));
        assert_eq!(list, items(&["b", "a", "c"]));
        move_item(&mut list, Some(3.0), Some(0.0));
        move_item(&mut list, Some(0.5), Some(1.0));
        move_item(&mut list, Some(0.0), Some(f64::NAN));
        assert_eq!(list, items(&["b", "a", "c"]));
    }

    #[test]
    fn removes_one_position() {
        let mut list = items(&["x", "y", "x"]);
        remove_at(&mut list, Some(2.0));
        remove_at(&mut list, Some(5.0));
        remove_at(&mut list, None);
        assert_eq!(list, items(&["x", "y"]));
    }
}
