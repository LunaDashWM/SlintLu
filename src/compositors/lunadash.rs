//! LunaDash: JSON requests on its control socket (`$LUNADASH_CONTROL`, or
//! `$XDG_RUNTIME_DIR/lunadash-0-control`), as sway's IPC: `get-*` requests for each topic
//! and a `subscribe` stream of events about them.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::Backend;
use super::model::{
    Action, DesktopState, KeyBinding, SwitcherItem, SwitcherState, WallpaperEntry, Window,
    Workspace,
};
use crate::util::uri::path_from_path_or_uri;

/// The events the shell follows.
const EVENTS: [&str; 6] =
    ["workspace", "window", "appearance", "wallpaper", "keybindings", "shutdown"];
// A reply may wait for the compositor to finish a long frame: giving up early
// only hides the outcome, since the request is already sent.
const TIMEOUT: Duration = Duration::from_millis(5000);

pub struct LunaDash {
    socket: PathBuf,
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        // SAFETY: getuid has no preconditions and cannot fail.
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}

impl LunaDash {
    pub fn from_env() -> Option<Self> {
        let explicit = ["LUNADASH_CONTROL", "LUDASH_CONTROL"]
            .iter()
            .find_map(std::env::var_os)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let socket = explicit.or_else(|| {
            ["lunadash-0-control", "ludash-0-control"]
                .iter()
                .map(|n| runtime_dir().join(n))
                .find(|p| p.exists())
        })?;
        Some(Self { socket })
    }

    /// Sends one request and returns the parsed response.
    pub fn request(&self, method: &str, value: &str) -> io::Result<Value> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        writeln!(stream, "{}", json!({ "method": method, "value": value }))?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        let response: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        if let Some(error) = response.get("error").and_then(Value::as_str) {
            return Err(io::Error::other(format!("LunaDash: {error}")));
        }
        Ok(response)
    }

    /// The topics the shell follows, read with their `get-*` requests into one object.
    fn snapshot(&self) -> io::Result<Value> {
        let mut status = self.request("get-workspaces", "")?;
        for topic in ["get-clients", "get-wallpaper", "get-keybindings"] {
            merge(&mut status, self.request(topic, "")?);
        }
        status["appearance"] = self.request("get-appearance", "")?["appearance"].take();
        Ok(status)
    }
}

fn merge(into: &mut Value, from: Value) {
    if let (Some(into), Value::Object(from)) = (into.as_object_mut(), from) {
        into.extend(from);
    }
}

/// Applies one `subscribe` event to the topics `snapshot` read.
fn apply_event(status: &mut Value, event: &Value) {
    match event["event"].as_str() {
        Some("workspace") => {
            status["workspace"] = event["current"].clone();
            status["workspaceCount"] = event["workspaceCount"].clone();
        }
        Some("window") => {
            let container = &event["container"];
            if !status["clients"].is_array() {
                status["clients"] = json!([]);
            }
            let Some(clients) = status["clients"].as_array_mut() else { return };
            let found = clients.iter().position(|c| c["id"] == container["id"]);
            if event["change"] == "close" {
                if let Some(index) = found {
                    clients.remove(index);
                }
                return;
            }
            if container["focused"] == true {
                for client in clients.iter_mut() {
                    client["focused"] = json!(false);
                }
            }
            match found {
                Some(index) => clients[index] = container.clone(),
                None => clients.push(container.clone()),
            }
        }
        Some("appearance") => status["appearance"] = event["appearance"].clone(),
        Some("wallpaper") => merge(status, event["wallpaper"].clone()),
        Some("keybindings") => status["keybindings"] = event["keybindings"].clone(),
        Some("shutdown") => status["shutdown"] = json!(true),
        _ => {}
    }
}

/// Builds the shell's view from the topics `snapshot` read.
fn build_state(status: &Value) -> DesktopState {
    let active = status["workspace"].as_i64().unwrap_or(0);
    // LunaDash adds and removes workspaces as windows come and go.
    let count = status["workspaceCount"].as_i64().unwrap_or(0).max(active + 1);
    let output = status["outputs"]
        .as_array()
        .and_then(|o| o.first())
        .and_then(|o| o["name"].as_str())
        .unwrap_or_default()
        .to_owned();
    let mut state = DesktopState {
        workspaces: (0..count)
            .map(|index| Workspace {
                id: index as u64,
                index: index + 1,
                name: String::new(),
                output: output.clone(),
                active: index == active,
                focused: index == active,
                urgent: false,
                windows: 0,
            })
            .collect(),
        windows: status["clients"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["mapped"].as_bool().unwrap_or(true))
            .filter_map(|c| {
                Some(Window {
                    id: c["id"].as_u64()?,
                    title: c["title"].as_str().unwrap_or_default().to_owned(),
                    app_id: c["appId"].as_str().unwrap_or_default().to_owned(),
                    workspace: c["workspace"].as_u64(),
                    position: c["x"].as_i64().zip(c["y"].as_i64())
                        .map(|(x, y)| (x as i32, y as i32)),
                    focused: c["focused"].as_bool().unwrap_or(false),
                    urgent: false,
                })
            })
            .collect(),
        keyboard_layout: String::new(),
        wallpaper: status["wallpaperImage"]
            .as_str()
            .map(|uri| path_from_path_or_uri(uri).to_string_lossy().into_owned())
            .unwrap_or_default(),
        wallpaper_poster: status["wallpaperPoster"]
            .as_str()
            .map(|uri| path_from_path_or_uri(uri).to_string_lossy().into_owned())
            .unwrap_or_default(),
        wallpaper_live: status["wallpaperLive"].as_bool().unwrap_or(false),
        wallpapers: wallpaper_library(&status["wallpapers"]),
        wallpaper_directory: status["wallpapers"]["directory"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        preferences: status
            .get("appearance")
            .filter(|p| p.is_object())
            .map(|p| p.to_string())
            .unwrap_or_default(),
        language: status["language"].as_str().unwrap_or_default().to_owned(),
        session_ending: status["shutdown"].as_bool().unwrap_or(false),
        bindings: bindings(&status["keybindings"]),
    };
    state.normalize();
    state
}

/// LunaDash's key bindings (`{"normal": {"Meta+Q": "close-window", …}, "overview": {…}}`):
/// normal mode first, then by action and keys.
fn bindings(modes: &Value) -> Vec<KeyBinding> {
    let mut list: Vec<KeyBinding> = modes
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(mode, keys)| {
            keys.as_object().into_iter().flatten().filter_map(move |(keys, action)| {
                Some(KeyBinding {
                    mode: mode.clone(),
                    keys: split_keys(keys),
                    action: action.as_str().filter(|a| !a.is_empty())?.to_owned(),
                })
            })
        })
        .collect();
    list.sort_by(|a, b| {
        (a.mode != "normal", &a.mode, &a.action, &a.keys).cmp(&(
            b.mode != "normal",
            &b.mode,
            &b.action,
            &b.keys,
        ))
    });
    list
}

/// `"Meta+Shift+S"` → `["Meta", "Shift", "S"]`; a `+` key stays one key (`"Meta++"`).
fn split_keys(keys: &str) -> Vec<String> {
    let (modifiers, key) = match keys.strip_suffix("++") {
        Some(rest) => (rest, "+"),
        None => keys.rsplit_once('+').unwrap_or(("", keys)),
    };
    modifiers
        .split('+')
        .filter(|part| !part.is_empty())
        .chain(std::iter::once(key))
        .map(str::to_owned)
        .collect()
}

/// The entries of LunaDash's wallpaper library snapshot.
fn wallpaper_library(snapshot: &Value) -> Vec<WallpaperEntry> {
    let local = |v: &Value| {
        v.as_str()
            .filter(|s| !s.is_empty())
            .map(|uri| path_from_path_or_uri(uri).to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    snapshot["library"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let path = entry["path"].as_str().filter(|p| !p.is_empty())?.to_owned();
            let video = entry["type"].as_str() == Some("video");
            let preview = local(&entry["preview"]);
            Some(WallpaperEntry {
                name: entry["name"].as_str().unwrap_or_default().to_owned(),
                category: entry["category"].as_str().unwrap_or_default().to_owned(),
                video,
                bundled: entry["bundled"].as_bool().unwrap_or(false),
                favorite: entry["favorite"].as_bool().unwrap_or(false),
                preview: if preview.is_empty() && !video { path.clone() } else { preview },
                path,
            })
        })
        .collect()
}

/// Builds the switcher state from LunaDash's interaction snapshot.
fn build_switcher(snapshot: &Value) -> SwitcherState {
    let workspaces = snapshot["scope"].as_str() == Some("workspaces");
    let list = if workspaces { &snapshot["workspaces"] } else { &snapshot["windows"] };
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
    let items = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item["id"].as_u64()?;
            let windows = item["windows"].as_array();
            Some(SwitcherItem {
                id,
                title: text(item, "title"),
                app_id: text(item, "appId"),
                // Workspace entries are numbered from 1; windows report a 0-based workspace.
                workspace: if workspaces {
                    id - 1
                } else {
                    item["workspace"].as_u64().unwrap_or(0)
                },
                focused: item["focused"].as_bool().unwrap_or(false),
                minimized: item["minimized"].as_bool().unwrap_or(false),
                apps: windows.into_iter().flatten().map(|w| text(w, "appId")).collect(),
                preview: item["thumbnail"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|uri| path_from_path_or_uri(uri).to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let index = snapshot["index"].as_u64().unwrap_or(0) as usize;
    SwitcherState {
        active: snapshot["active"].as_bool().unwrap_or(false) && !items.is_empty(),
        workspaces,
        index: index.min(items.len().saturating_sub(1)),
        items,
    }
}

/// Reads the interaction snapshot LunaDash writes next to its control socket.
fn read_switcher(path: &std::path::Path) -> SwitcherState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|snapshot| build_switcher(&snapshot))
        .unwrap_or_default()
}

impl Backend for LunaDash {
    /// Subscribes first, then reads the topics: what changes meanwhile arrives as events.
    fn watch(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        let stream = UnixStream::connect(&self.socket)?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        let events = json!(EVENTS).to_string();
        writeln!(&stream, "{}", json!({ "method": "subscribe", "value": events }))?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let reply: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        if reply["success"] != true {
            let error = reply["error"].as_str().unwrap_or("cannot subscribe");
            return Err(io::Error::other(format!("LunaDash: {error}")));
        }
        let mut status = self.snapshot()?;
        let mut last = build_state(&status);
        on_state(&last);
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "LunaDash closed the stream",
                ));
            }
            let event: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
            apply_event(&mut status, &event);
            let state = build_state(&status);
            if state != last {
                on_state(&state);
                last = state;
            }
        }
    }

    fn perform(&self, action: &Action) -> io::Result<()> {
        match action {
            Action::FocusWorkspace(id) => self.request("workspace", &id.to_string()),
            // Activating also restores a minimized window and switches to its workspace.
            Action::FocusWindow(id) => self.request("activate-window", &id.to_string()),
            Action::CloseWindow(id) => self.request("close", &id.to_string()),
            Action::OpenSwitcher { workspaces } => {
                self.request("switch-begin", if *workspaces { "workspaces" } else { "windows" })
            }
            Action::StepSwitcher(delta) => {
                let direction = if *delta < 0 { "previous" } else { "next" };
                for _ in 1..delta.unsigned_abs() {
                    self.request("switch-step", direction)?;
                }
                self.request("switch-step", direction)
            }
            Action::ChooseSwitcher(id) => {
                self.request("switch-window", &id.to_string())?;
                self.request("switch-accept", "")
            }
            Action::FinishSwitcher { accept } => {
                self.request(if *accept { "switch-accept" } else { "switch-cancel" }, "")
            }
        }
        .map(drop)
    }

    fn watch_switcher(&self, on_state: &mut dyn FnMut(&SwitcherState)) -> io::Result<()> {
        use rustix::fs::inotify;
        let mut path = self.socket.clone().into_os_string();
        path.push("-interaction.json");
        let path = PathBuf::from(path);
        let directory = path.parent().map(PathBuf::from).unwrap_or_default();
        let name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
        // LunaDash replaces the file atomically: watch the directory for it being moved in.
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC)?;
        inotify::add_watch(
            &fd,
            &directory,
            inotify::WatchFlags::MOVED_TO | inotify::WatchFlags::CLOSE_WRITE,
        )?;
        let mut last = read_switcher(&path);
        on_state(&last);
        let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
        let mut reader = inotify::Reader::new(&fd, &mut buffer);
        loop {
            let event = reader.next()?;
            if event.file_name().is_none_or(|n| n.to_bytes() != name.as_encoded_bytes()) {
                continue;
            }
            let state = read_switcher(&path);
            if state != last {
                on_state(&state);
                last = state;
            }
        }
    }

    fn command(&self, method: &str, value: &str) -> io::Result<String> {
        self.request(method, value).map(|v| v.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_state_from_topics() {
        let status = json!({
            "workspace": 2,
            "workspaceCount": 10,
            "clients": [
                {"id": 7, "title": "Zed", "appId": "dev.zed.Zed", "workspace": 2, "focused": true, "mapped": true},
                {"id": 8, "title": "Term", "appId": "foot", "workspace": 0, "focused": false, "mapped": true},
                {"id": 9, "title": "hidden", "appId": "x", "workspace": 0, "mapped": false}
            ],
            "wallpaperImage": "file:///usr/share/backgrounds/Night%20Sky.png",
            "language": "zh_TW",
            "keybindings": {
                "overview": {"Escape": "close-overview"},
                "normal": {"Meta+Shift+S": "screenshot", "Meta+Q": "close-window",
                           "Meta++": "set-column-width +10%", "Meta+T": ""}
            }
        });
        let state = build_state(&status);
        assert_eq!(state.workspaces.len(), 10);
        assert!(state.workspaces[2].active);
        assert_eq!(state.workspaces[2].index, 3);
        assert_eq!(state.workspaces[0].windows, 1);
        assert_eq!(state.windows.len(), 2);
        assert_eq!(state.focused_window().unwrap().title, "Zed");
        assert_eq!(state.wallpaper, "/usr/share/backgrounds/Night Sky.png");
        assert_eq!(state.language, "zh_TW");
        let keys = |action: &str| {
            state.bindings.iter().find(|b| b.action == action).map(|b| b.keys.join(" "))
        };
        assert_eq!(state.bindings.len(), 4, "an empty action binds nothing");
        assert_eq!(state.bindings[0].action, "close-window");
        assert_eq!(state.bindings[3].mode, "overview", "overview bindings come last");
        assert_eq!(keys("screenshot").as_deref(), Some("Meta Shift S"));
        assert_eq!(keys("set-column-width +10%").as_deref(), Some("Meta +"));
        assert!(!state.session_ending);
        assert!(build_state(&json!({"shutdown": true})).session_ending);
    }

    #[test]
    fn talks_the_control_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lunadash-0-control");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request, json!({"method": "workspace", "value": "3"}));
            writeln!(reader.get_mut(), "{}", json!({"success": true})).unwrap();
        });
        let backend = LunaDash { socket: path };
        backend.perform(&Action::FocusWorkspace(3)).unwrap();
    }

    #[test]
    fn follows_the_dynamic_workspace_count() {
        let state = build_state(&json!({"workspace": 1, "workspaceCount": 3}));
        assert_eq!(state.workspaces.len(), 3);
        assert!(state.workspaces[1].active);
        assert_eq!(build_state(&json!({"workspace": 2})).workspaces.len(), 3);
        let video = build_state(&json!({
            "wallpaperPoster": "file:///tmp/poster.png",
            "wallpaperLive": true,
        }));
        assert_eq!(video.wallpaper_poster, "/tmp/poster.png");
        assert!(video.wallpaper_live);
    }

    #[test]
    fn reads_the_wallpaper_library_and_preferences() {
        let state = build_state(&json!({
            "appearance": {"gap": 12, "tilingMode": "scroll"},
            "wallpapers": {
                "directory": "/home/u/Pictures/Wallpapers",
                "library": [
                    {"path": "/w/a.png", "name": "a", "category": "Nature", "type": "image",
                     "preview": "file:///w/a.png", "favorite": true},
                    {"path": "/w/b.mp4", "name": "b", "category": "Live", "type": "video",
                     "preview": "", "bundled": false},
                    {"name": "no path"},
                ],
            },
        }));
        assert_eq!(state.wallpaper_directory, "/home/u/Pictures/Wallpapers");
        assert_eq!(state.wallpapers.len(), 2, "entries without a path are skipped");
        assert_eq!(state.wallpapers[0].preview, "/w/a.png");
        assert!(state.wallpapers[0].favorite && !state.wallpapers[0].video);
        assert!(state.wallpapers[1].video);
        assert_eq!(state.wallpapers[1].preview, "", "videos without a poster have no preview");
        let preferences: Value = serde_json::from_str(&state.preferences).unwrap();
        assert_eq!(preferences["tilingMode"], "scroll");
        assert_eq!(build_state(&json!({})).preferences, "");
    }

    #[test]
    fn follows_events() {
        let mut status = json!({
            "workspace": 0, "workspaceCount": 2,
            "clients": [{"id": 1, "title": "a", "appId": "foot", "workspace": 0, "focused": true}],
        });
        let window = |change: &str, container: Value| json!({"event": "window", "change": change, "container": container});
        apply_event(&mut status, &window("new", json!({"id": 2, "title": "b", "focused": true})));
        let state = build_state(&status);
        assert_eq!(state.windows.len(), 2);
        assert_eq!(state.focused_window().unwrap().id, 2, "a focused window unfocuses the rest");
        apply_event(&mut status, &window("title", json!({"id": 1, "title": "renamed"})));
        assert_eq!(build_state(&status).windows[0].title, "renamed", "updates keep their place");
        apply_event(&mut status, &window("close", json!({"id": 2})));
        assert_eq!(build_state(&status).windows.len(), 1);
        apply_event(
            &mut status,
            &json!({"event": "workspace", "change": "focus", "current": 2, "old": 0, "workspaceCount": 4}),
        );
        let state = build_state(&status);
        assert_eq!(state.workspaces.len(), 4);
        assert!(state.workspaces[2].active);
        apply_event(
            &mut status,
            &json!({"event": "wallpaper", "change": "update",
                    "wallpaper": {"wallpaperImage": "file:///w/new.png", "wallpaperLive": false}}),
        );
        assert_eq!(build_state(&status).wallpaper, "/w/new.png");
        apply_event(&mut status, &json!({"event": "shutdown", "change": "exit"}));
        assert!(build_state(&status).session_ending);
    }

    #[test]
    fn reads_the_switcher_snapshot() {
        let windows = build_switcher(&json!({
            "active": true, "scope": "windows", "index": 1,
            "windows": [
                {"id": 4, "title": "Files", "appId": "org.gnome.Nautilus", "workspace": 0},
                {"id": 7, "title": "Term", "appId": "foot", "workspace": 2, "minimized": true},
            ],
        }));
        assert!(windows.active && !windows.workspaces);
        assert_eq!(windows.index, 1);
        assert_eq!(windows.items[1].workspace, 2);
        assert!(windows.items[1].minimized);

        let workspaces = build_switcher(&json!({
            "active": true, "scope": "workspaces", "index": 9,
            "workspaces": [{"id": 1, "windows": [{"appId": "foot"}]}, {"id": 2, "windows": []}],
        }));
        assert!(workspaces.workspaces);
        assert_eq!(workspaces.index, 1, "the index is clamped to the entries");
        assert_eq!(workspaces.items[0].workspace, 0);
        assert_eq!(workspaces.items[0].apps, vec!["foot".to_owned()]);
        assert!(!build_switcher(&json!({"active": true, "windows": []})).active);
    }
}
