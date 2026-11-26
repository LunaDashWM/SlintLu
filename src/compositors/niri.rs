//! niri: JSON over `$NIRI_SOCKET`, kept up to date with its event stream.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::Backend;
use super::model::{Action, DesktopState, Window, Workspace};

pub struct Niri {
    socket: PathBuf,
}

impl Niri {
    pub fn from_env() -> Option<Self> {
        std::env::var_os("NIRI_SOCKET").map(|s| Self { socket: s.into() })
    }

    fn request(&self, request: &Value) -> io::Result<UnixStream> {
        let mut stream = UnixStream::connect(&self.socket)?;
        writeln!(stream, "{request}")?;
        stream.flush()?;
        Ok(stream)
    }
}

impl Backend for Niri {
    fn watch(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        let stream = self.request(&json!("EventStream"))?;
        let mut state = NiriState::default();
        for line in BufReader::new(stream).lines() {
            let event: Value = match serde_json::from_str(&line?) {
                Ok(event) => event,
                Err(e) => {
                    log::debug!("niri: unparsable event: {e}");
                    continue;
                }
            };
            if state.apply(&event) {
                on_state(&state.snapshot());
            }
        }
        Err(io::Error::new(io::ErrorKind::UnexpectedEof, "niri closed the event stream"))
    }

    fn perform(&self, action: &Action) -> io::Result<()> {
        let action = match action {
            Action::FocusWorkspace(id) => json!({"FocusWorkspace": {"reference": {"Id": id}}}),
            Action::FocusWindow(id) => json!({"FocusWindow": {"id": id}}),
            Action::CloseWindow(id) => json!({"CloseWindow": {"id": id}}),
            other => return Err(super::unsupported(other)),
        };
        let stream = self.request(&json!({ "Action": action }))?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply)?;
        if reply.contains("\"Err\"") {
            return Err(io::Error::other(format!("niri refused the action: {}", reply.trim())));
        }
        Ok(())
    }
}

/// niri state rebuilt from events.
#[derive(Default)]
struct NiriState {
    workspaces: Vec<Workspace>,
    windows: Vec<Window>,
    layouts: Vec<String>,
    layout: usize,
}

fn workspace(v: &Value) -> Option<Workspace> {
    Some(Workspace {
        id: v["id"].as_u64()?,
        index: v["idx"].as_i64().unwrap_or(0),
        name: v["name"].as_str().unwrap_or_default().to_owned(),
        output: v["output"].as_str().unwrap_or_default().to_owned(),
        active: v["is_active"].as_bool().unwrap_or(false),
        focused: v["is_focused"].as_bool().unwrap_or(false),
        urgent: v["is_urgent"].as_bool().unwrap_or(false),
        windows: 0,
    })
}

fn window(v: &Value) -> Option<Window> {
    Some(Window {
        id: v["id"].as_u64()?,
        title: v["title"].as_str().unwrap_or_default().to_owned(),
        app_id: v["app_id"].as_str().unwrap_or_default().to_owned(),
        workspace: v["workspace_id"].as_u64(),
        position: v["layout"]["pos_in_scrolling_layout"].as_array().and_then(|p| {
            Some((p.first()?.as_i64()? as i32, p.get(1)?.as_i64()? as i32))
        }),
        focused: v["is_focused"].as_bool().unwrap_or(false),
        icon_file: None,
        urgent: v["is_urgent"].as_bool().unwrap_or(false),
    })
}

impl NiriState {
    /// Applies one event; returns whether anything a shell shows changed.
    fn apply(&mut self, event: &Value) -> bool {
        let Some((name, body)) = event.as_object().and_then(|o| o.iter().next()) else {
            return false;
        };
        match name.as_str() {
            "WorkspacesChanged" => {
                self.workspaces = body["workspaces"]
                    .as_array()
                    .map(|a| a.iter().filter_map(workspace).collect())
                    .unwrap_or_default();
            }
            "WorkspaceActivated" => {
                let (Some(id), focused) = (body["id"].as_u64(), body["focused"].as_bool()) else {
                    return false;
                };
                let output = self.workspaces.iter().find(|w| w.id == id).map(|w| w.output.clone());
                for ws in &mut self.workspaces {
                    if Some(&ws.output) == output.as_ref() {
                        ws.active = ws.id == id;
                    }
                    if focused == Some(true) {
                        ws.focused = ws.id == id;
                    }
                }
            }
            "WorkspaceUrgencyChanged" => {
                if let Some(ws) =
                    self.workspaces.iter_mut().find(|w| Some(w.id) == body["id"].as_u64())
                {
                    ws.urgent = body["urgent"].as_bool().unwrap_or(false);
                }
            }
            "WindowsChanged" => {
                self.windows = body["windows"]
                    .as_array()
                    .map(|a| a.iter().filter_map(window).collect())
                    .unwrap_or_default();
            }
            "WindowOpenedOrChanged" => {
                let Some(new) = window(&body["window"]) else { return false };
                if new.focused {
                    self.windows.iter_mut().for_each(|w| w.focused = false);
                }
                match self.windows.iter_mut().find(|w| w.id == new.id) {
                    Some(existing) => *existing = new,
                    None => self.windows.push(new),
                }
            }
            "WindowClosed" => self.windows.retain(|w| Some(w.id) != body["id"].as_u64()),
            "WindowFocusChanged" => {
                let id = body["id"].as_u64();
                self.windows.iter_mut().for_each(|w| w.focused = Some(w.id) == id);
            }
            "WindowUrgencyChanged" => {
                if let Some(w) = self.windows.iter_mut().find(|w| Some(w.id) == body["id"].as_u64())
                {
                    w.urgent = body["urgent"].as_bool().unwrap_or(false);
                }
            }
            "KeyboardLayoutsChanged" => {
                let layouts = &body["keyboard_layouts"];
                self.layouts = layouts["names"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                self.layout = layouts["current_idx"].as_u64().unwrap_or(0) as usize;
            }
            "KeyboardLayoutSwitched" => self.layout = body["idx"].as_u64().unwrap_or(0) as usize,
            _ => return false,
        }
        true
    }

    fn snapshot(&self) -> DesktopState {
        let mut state = DesktopState {
            workspaces: self.workspaces.clone(),
            windows: self.windows.clone(),
            keyboard_layout: self.layouts.get(self.layout).cloned().unwrap_or_default(),
            ..DesktopState::default()
        };
        state.normalize();
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(state: &mut NiriState, event: &str) -> bool {
        state.apply(&serde_json::from_str(event).unwrap())
    }

    #[test]
    fn follows_workspace_and_window_events() {
        let mut s = NiriState::default();
        assert!(apply(
            &mut s,
            r#"{"WorkspacesChanged":{"workspaces":[
            {"id":2,"idx":2,"name":null,"output":"HDMI-A-1","is_urgent":false,"is_active":false,"is_focused":false},
            {"id":1,"idx":1,"name":"web","output":"HDMI-A-1","is_urgent":false,"is_active":true,"is_focused":true}]}}"#
        ));
        assert!(apply(
            &mut s,
            r#"{"WindowsChanged":{"windows":[
            {"id":5,"title":"Docs","app_id":"firefox","workspace_id":1,"is_focused":true,"is_urgent":false}]}}"#
        ));
        assert!(apply(
            &mut s,
            r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{"names":["English (US)","German"],"current_idx":1}}}"#
        ));

        let snap = s.snapshot();
        assert_eq!(snap.workspaces[0].name, "web");
        assert_eq!(snap.workspaces[0].windows, 1);
        assert_eq!(snap.focused_window().unwrap().title, "Docs");
        assert_eq!(snap.keyboard_layout, "German");

        assert!(apply(&mut s, r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#));
        let snap = s.snapshot();
        assert!(snap.workspaces[1].active && snap.workspaces[1].focused);
        assert!(!snap.workspaces[0].active && !snap.workspaces[0].focused);

        assert!(apply(
            &mut s,
            r#"{"WindowOpenedOrChanged":{"window":{"id":6,"title":"Term","app_id":"foot","workspace_id":2,"is_focused":true}}}"#
        ));
        assert_eq!(s.snapshot().focused_window().unwrap().id, 6);
        assert!(apply(&mut s, r#"{"WindowClosed":{"id":6}}"#));
        assert!(apply(&mut s, r#"{"WindowFocusChanged":{"id":null}}"#));
        assert!(s.snapshot().focused_window().is_none());
        assert!(!apply(&mut s, r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#));
    }
}
