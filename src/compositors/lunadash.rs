//! LunaDash: JSON requests on its control socket (`$LUNADASH_CONTROL`, or
//! `$XDG_RUNTIME_DIR/lunadash-0-control`). LunaDash has no event stream, so the state is
//! polled like its own shell does.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::Backend;
use super::model::{
    Action, DesktopState, Shortcut, SwitcherItem, SwitcherState, WallpaperEntry, Window, Workspace,
};
use crate::util::uri::path_from_path_or_uri;

/// The number of workspaces of LunaDash versions that do not report it (they had ten).
const DEFAULT_WORKSPACES: i64 = 10;
const POLL: Duration = Duration::from_millis(700);
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
}

/// Builds the shell's view from a `status` response.
fn build_state(status: &Value) -> DesktopState {
    let active = status["workspace"].as_i64().unwrap_or(0);
    // LunaDash adds and removes workspaces as windows come and go.
    let count = status["workspaceCount"].as_i64().unwrap_or(DEFAULT_WORKSPACES).max(active + 1);
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
        shortcuts: shortcuts(&status["shortcuts"]),
    };
    state.normalize();
    state
}

/// LunaDash's shortcut bindings (`{"closeWindow": "Meta+Q", …}`), sorted by action.
fn shortcuts(bindings: &Value) -> Vec<Shortcut> {
    let mut list: Vec<Shortcut> = bindings
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(action, keys)| {
            let keys = keys.as_str()?;
            (!keys.is_empty() && keys != "Disabled")
                .then(|| Shortcut { action: action.clone(), keys: split_keys(keys) })
        })
        .collect();
    list.sort_by(|a, b| a.action.cmp(&b.action));
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

impl LunaDash {
    /// Follows LunaDash's `subscribe` stream: its shell state now and on every change.
    /// `Unsupported` when this LunaDash predates the stream.
    fn subscribe(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        let stream = UnixStream::connect(&self.socket)?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        writeln!(&stream, "{}", json!({ "method": "subscribe", "value": "shell" }))?;
        let mut reader = BufReader::new(stream);
        let mut last: Option<DesktopState> = None;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "LunaDash closed the stream",
                ));
            }
            let status: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
            if last.is_none() && status.get("error").is_some() {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "no subscribe stream"));
            }
            let state = build_state(&status);
            if last.as_ref() != Some(&state) {
                on_state(&state);
                last = Some(state);
            }
        }
    }

    /// Asks for the full status again and again (LunaDash without the stream).
    fn poll(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        let mut last: Option<DesktopState> = None;
        loop {
            let state = build_state(&self.request("status", "")?);
            if last.as_ref() != Some(&state) {
                on_state(&state);
                last = Some(state);
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Backend for LunaDash {
    fn watch(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        match self.subscribe(on_state) {
            Err(e) if e.kind() == io::ErrorKind::Unsupported => self.poll(on_state),
            result => result,
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
    fn builds_state_from_status() {
        let status = json!({
            "workspace": 2,
            "clients": [
                {"id": 7, "title": "Zed", "appId": "dev.zed.Zed", "workspace": 2, "focused": true, "mapped": true},
                {"id": 8, "title": "Term", "appId": "foot", "workspace": 0, "focused": false, "mapped": true},
                {"id": 9, "title": "hidden", "appId": "x", "workspace": 0, "mapped": false}
            ],
            "wallpaperImage": "file:///usr/share/backgrounds/Night%20Sky.png",
            "language": "zh_TW",
            "shortcuts": {"screenshot": "Meta+Shift+S", "closeWindow": "Meta+Q",
                          "widenColumn": "Meta++", "launchFiles": "Disabled"}
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
            state.shortcuts.iter().find(|s| s.action == action).map(|s| s.keys.join(" "))
        };
        assert_eq!(state.shortcuts.len(), 3);
        assert_eq!(state.shortcuts[0].action, "closeWindow");
        assert_eq!(keys("screenshot").as_deref(), Some("Meta Shift S"));
        assert_eq!(keys("widenColumn").as_deref(), Some("Meta +"));
        assert_eq!(keys("launchFiles"), None);
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
            writeln!(reader.get_mut(), "{}", json!({"workspace": 3, "clients": []})).unwrap();
        });
        let backend = LunaDash { socket: path };
        backend.perform(&Action::FocusWorkspace(3)).unwrap();
    }

    #[test]
    fn follows_the_dynamic_workspace_count() {
        let state = build_state(&json!({"workspace": 1, "workspaceCount": 3}));
        assert_eq!(state.workspaces.len(), 3);
        assert!(state.workspaces[1].active);
        // Older LunaDash versions had ten fixed workspaces.
        assert_eq!(build_state(&json!({"workspace": 0})).workspaces.len(), 10);
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
