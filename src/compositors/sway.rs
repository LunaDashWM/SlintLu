//! Sway (and i3-compatible compositors): the i3 IPC binary protocol on `$SWAYSOCK`.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde_json::Value;

use super::Backend;
use super::model::{Action, DesktopState, Window, Workspace};

const MAGIC: &[u8; 6] = b"i3-ipc";
const RUN_COMMAND: u32 = 0;
const GET_WORKSPACES: u32 = 1;
const SUBSCRIBE: u32 = 2;
const GET_TREE: u32 = 4;
const GET_INPUTS: u32 = 100;

pub struct Sway {
    socket: PathBuf,
}

fn send(stream: &mut UnixStream, kind: u32, payload: &str) -> io::Result<()> {
    let mut message = Vec::with_capacity(14 + payload.len());
    message.extend_from_slice(MAGIC);
    message.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    message.extend_from_slice(&kind.to_ne_bytes());
    message.extend_from_slice(payload.as_bytes());
    stream.write_all(&message)
}

/// Reads one message; returns its type and JSON payload.
fn receive(stream: &mut UnixStream) -> io::Result<(u32, Value)> {
    let mut header = [0u8; 14];
    stream.read_exact(&mut header)?;
    if &header[..6] != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not an i3-ipc message"));
    }
    let len = u32::from_ne_bytes(header[6..10].try_into().unwrap_or_default()) as usize;
    let kind = u32::from_ne_bytes(header[10..14].try_into().unwrap_or_default());
    let mut payload = vec![0; len];
    stream.read_exact(&mut payload)?;
    let value = serde_json::from_slice(&payload).map_err(io::Error::other)?;
    Ok((kind, value))
}

impl Sway {
    pub fn from_env() -> Option<Self> {
        std::env::var_os("SWAYSOCK")
            .or_else(|| std::env::var_os("I3SOCK"))
            .map(|s| Self { socket: s.into() })
    }

    fn query(&self, kind: u32, payload: &str) -> io::Result<Value> {
        let mut stream = UnixStream::connect(&self.socket)?;
        send(&mut stream, kind, payload)?;
        receive(&mut stream).map(|(_, v)| v)
    }

    fn snapshot(&self) -> io::Result<DesktopState> {
        Ok(build_state(
            &self.query(GET_WORKSPACES, "")?,
            &self.query(GET_TREE, "")?,
            &self.query(GET_INPUTS, "").unwrap_or(Value::Null),
        ))
    }
}

/// Collects application windows below `node`, remembering the workspace they are on.
fn collect_windows(node: &Value, workspace: Option<u64>, out: &mut Vec<Window>) {
    let workspace = if node["type"] == "workspace" { node["id"].as_u64() } else { workspace };
    let is_window = node["type"] == "con" || node["type"] == "floating_con";
    if is_window && (node["pid"].is_u64() || node["window"].is_u64()) {
        let app_id = node["app_id"]
            .as_str()
            .or_else(|| node["window_properties"]["class"].as_str())
            .unwrap_or_default();
        out.push(Window {
            id: node["id"].as_u64().unwrap_or(0),
            title: node["name"].as_str().unwrap_or_default().to_owned(),
            app_id: app_id.to_owned(),
            workspace,
            position: node["rect"]["x"].as_i64().zip(node["rect"]["y"].as_i64())
                .map(|(x, y)| (x as i32, y as i32)),
            focused: node["focused"].as_bool().unwrap_or(false),
            icon_file: None,
            urgent: node["urgent"].as_bool().unwrap_or(false),
        });
    }
    for child in
        node["nodes"].as_array().into_iter().chain(node["floating_nodes"].as_array()).flatten()
    {
        collect_windows(child, workspace, out);
    }
}

fn build_state(workspaces: &Value, tree: &Value, inputs: &Value) -> DesktopState {
    let mut windows = Vec::new();
    collect_windows(tree, None, &mut windows);
    let mut state = DesktopState {
        workspaces: workspaces
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                Some(Workspace {
                    id: w["id"].as_u64()?,
                    index: w["num"].as_i64().unwrap_or(-1),
                    name: w["name"].as_str().unwrap_or_default().to_owned(),
                    output: w["output"].as_str().unwrap_or_default().to_owned(),
                    active: w["visible"].as_bool().unwrap_or(false),
                    focused: w["focused"].as_bool().unwrap_or(false),
                    urgent: w["urgent"].as_bool().unwrap_or(false),
                    windows: 0,
                })
            })
            .collect(),
        windows,
        keyboard_layout: inputs
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|i| i["xkb_active_layout_name"].as_str())
            .unwrap_or_default()
            .to_owned(),
        ..DesktopState::default()
    };
    state.normalize();
    state
}

impl Backend for Sway {
    fn watch(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        let mut events = UnixStream::connect(&self.socket)?;
        send(&mut events, SUBSCRIBE, r#"["workspace","window","input","output"]"#)?;
        let (_, reply) = receive(&mut events)?;
        if reply["success"] != true {
            return Err(io::Error::other("sway refused the subscription"));
        }
        let mut last = self.snapshot()?;
        on_state(&last);
        loop {
            receive(&mut events)?;
            let state = self.snapshot()?;
            if state != last {
                on_state(&state);
                last = state;
            }
        }
    }

    fn perform(&self, action: &Action) -> io::Result<()> {
        let command = match action {
            Action::FocusWorkspace(id) => {
                let workspaces = self.query(GET_WORKSPACES, "")?;
                let name = workspaces
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|w| w["id"].as_u64() == Some(*id))
                    .and_then(|w| w["name"].as_str())
                    .ok_or_else(|| io::Error::other(format!("no workspace with id {id}")))?
                    .to_owned();
                format!("workspace \"{}\"", name.replace('"', "\\\""))
            }
            Action::FocusWindow(id) => format!("[con_id={id}] focus"),
            Action::CloseWindow(id) => format!("[con_id={id}] kill"),
            other => return Err(super::unsupported(other)),
        };
        let reply = self.query(RUN_COMMAND, &command)?;
        match reply.as_array().and_then(|r| r.first()) {
            Some(result) if result["success"] == true => Ok(()),
            other => Err(io::Error::other(format!("sway: {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_state_from_tree() {
        let tree = json!({"type": "root", "nodes": [{"type": "output", "nodes": [
            {"type": "workspace", "id": 10, "nodes": [
                {"type": "con", "id": 11, "pid": 1, "name": "vim", "app_id": "foot", "focused": true, "nodes": []},
                {"type": "con", "id": 12, "nodes": [
                    {"type": "con", "id": 13, "window": 7, "name": "xterm", "app_id": null,
                     "window_properties": {"class": "XTerm"}, "nodes": []}
                ]}
            ], "floating_nodes": []}
        ]}]});
        let state = build_state(
            &json!([{"id": 10, "num": 1, "name": "1", "output": "eDP-1", "visible": true, "focused": true, "urgent": false}]),
            &tree,
            &json!([{"type": "keyboard", "xkb_active_layout_name": "English (US)"}]),
        );
        assert_eq!(state.windows.len(), 2);
        assert_eq!(state.windows[1].app_id, "XTerm");
        assert_eq!(state.workspaces[0].windows, 2);
        assert_eq!(state.focused_window().unwrap().title, "vim");
        assert_eq!(state.keyboard_layout, "English (US)");
    }

    #[test]
    fn message_framing_round_trips() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        send(&mut a, GET_WORKSPACES, "[1,2]").unwrap();
        let (kind, value) = receive(&mut b).unwrap();
        assert_eq!(kind, GET_WORKSPACES);
        assert_eq!(value, json!([1, 2]));
    }
}
