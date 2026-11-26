//! Hyprland: request socket for snapshots, `.socket2.sock` for change notifications.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde_json::Value;

use super::Backend;
use super::model::{Action, DesktopState, Window, Workspace};

/// Events after which the snapshot is refreshed.
const RELEVANT_EVENTS: &[&str] = &[
    "workspace",
    "workspacev2",
    "focusedmon",
    "focusedmonv2",
    "activewindow",
    "activewindowv2",
    "openwindow",
    "closewindow",
    "movewindow",
    "movewindowv2",
    "createworkspace",
    "createworkspacev2",
    "destroyworkspace",
    "destroyworkspacev2",
    "renameworkspace",
    "moveworkspace",
    "moveworkspacev2",
    "monitoradded",
    "monitorremoved",
    "urgent",
    "activelayout",
    "windowtitle",
    "windowtitlev2",
];

pub struct Hyprland {
    dir: PathBuf,
}

impl Hyprland {
    pub fn from_env() -> Option<Self> {
        let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let candidates = [
            runtime.map(|r| r.join("hypr").join(&signature)),
            Some(PathBuf::from("/tmp/hypr").join(&signature)),
        ];
        let dir = candidates.into_iter().flatten().find(|d| d.join(".socket.sock").exists())?;
        Some(Self { dir })
    }

    fn request(&self, command: &str) -> io::Result<String> {
        let mut stream = UnixStream::connect(self.dir.join(".socket.sock"))?;
        stream.write_all(command.as_bytes())?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply)?;
        Ok(reply)
    }

    fn json(&self, command: &str) -> Value {
        self.request(&format!("j/{command}"))
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or(Value::Null)
    }

    fn snapshot(&self) -> DesktopState {
        build_state(
            &self.json("workspaces"),
            &self.json("monitors"),
            &self.json("clients"),
            &self.json("activewindow"),
            &self.json("devices"),
        )
    }
}

fn address(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?.trim_start_matches("0x"), 16).ok()
}

fn build_state(
    workspaces: &Value,
    monitors: &Value,
    clients: &Value,
    active_window: &Value,
    devices: &Value,
) -> DesktopState {
    let monitors = monitors.as_array().cloned().unwrap_or_default();
    let visible =
        |id: i64| monitors.iter().any(|m| m["activeWorkspace"]["id"].as_i64() == Some(id));
    let focused_ws = monitors
        .iter()
        .find(|m| m["focused"].as_bool() == Some(true))
        .and_then(|m| m["activeWorkspace"]["id"].as_i64());
    let active_address = address(&active_window["address"]);

    let mut state = DesktopState {
        workspaces: workspaces
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                let id = w["id"].as_i64()?;
                // Special (scratchpad) workspaces have negative ids.
                (id > 0).then(|| Workspace {
                    id: id as u64,
                    index: id,
                    name: w["name"].as_str().unwrap_or_default().to_owned(),
                    output: w["monitor"].as_str().unwrap_or_default().to_owned(),
                    active: visible(id),
                    focused: focused_ws == Some(id),
                    urgent: false,
                    windows: 0,
                })
            })
            .collect(),
        windows: clients
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let id = address(&c["address"])?;
                Some(Window {
                    id,
                    title: c["title"].as_str().unwrap_or_default().to_owned(),
                    app_id: c["class"].as_str().unwrap_or_default().to_owned(),
                    workspace: c["workspace"]["id"].as_i64().filter(|i| *i > 0).map(|i| i as u64),
                    position: c["at"].as_array().and_then(|p| {
                        Some((p.first()?.as_i64()? as i32, p.get(1)?.as_i64()? as i32))
                    }),
                    focused: Some(id) == active_address,
                    icon_file: None,
                    urgent: false,
                })
            })
            .collect(),
        keyboard_layout: devices["keyboards"]
            .as_array()
            .and_then(|k| {
                k.iter().find(|k| k["main"].as_bool() == Some(true)).or_else(|| k.first())
            })
            .and_then(|k| k["active_keymap"].as_str())
            .unwrap_or_default()
            .to_owned(),
        ..DesktopState::default()
    };
    state.normalize();
    state
}

impl Backend for Hyprland {
    fn watch(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        let events = UnixStream::connect(self.dir.join(".socket2.sock"))?;
        let mut last = self.snapshot();
        on_state(&last);
        for line in BufReader::new(events).lines() {
            let line = line?;
            let event = line.split(">>").next().unwrap_or_default();
            if RELEVANT_EVENTS.contains(&event) {
                let state = self.snapshot();
                if state != last {
                    on_state(&state);
                    last = state;
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Hyprland closed the event socket"))
    }

    fn perform(&self, action: &Action) -> io::Result<()> {
        let command = match action {
            Action::FocusWorkspace(id) => format!("dispatch workspace {id}"),
            Action::FocusWindow(id) => format!("dispatch focuswindow address:0x{id:x}"),
            Action::CloseWindow(id) => format!("dispatch closewindow address:0x{id:x}"),
            other => return Err(super::unsupported(other)),
        };
        let reply = self.request(&command)?;
        if reply.trim() == "ok" {
            Ok(())
        } else {
            Err(io::Error::other(format!("Hyprland: {}", reply.trim())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_state_from_hyprctl_json() {
        let state = build_state(
            &json!([
                {"id": 1, "name": "1", "monitor": "DP-1"},
                {"id": 2, "name": "code", "monitor": "DP-1"},
                {"id": -98, "name": "special:scratch", "monitor": "DP-1"}
            ]),
            &json!([{"name": "DP-1", "focused": true, "activeWorkspace": {"id": 2}}]),
            &json!([
                {"address": "0x55a1", "title": "vim", "class": "foot", "workspace": {"id": 2}},
                {"address": "0x55b2", "title": "scratch", "class": "foot", "workspace": {"id": -98}}
            ]),
            &json!({"address": "0x55a1"}),
            &json!({"keyboards": [{"main": true, "active_keymap": "English (US)"}]}),
        );
        assert_eq!(state.workspaces.len(), 2);
        assert!(state.workspaces[1].active && state.workspaces[1].focused);
        assert_eq!(state.workspaces[1].windows, 1);
        assert_eq!(state.focused_window().unwrap().id, 0x55a1);
        assert_eq!(state.windows[1].workspace, None);
        assert_eq!(state.keyboard_layout, "English (US)");
    }
}
