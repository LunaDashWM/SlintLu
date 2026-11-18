//! A compositor seen through standard protocols plus its own adapter: workspaces, windows and
//! their actions come from the protocols ([`Protocols`]); the adapter adds what only that
//! compositor knows (how many windows each workspace holds, its wallpaper and preferences,
//! requests to the shell, its window switcher and commands).

use std::io;
use std::sync::Arc;
use std::sync::mpsc;

use super::Backend;
use super::model::{Action, DesktopState, SwitcherState};
use super::protocols::Protocols;

pub struct Composite {
    protocols: Arc<Protocols>,
    adapter: Arc<dyn Backend>,
}

enum Update {
    Protocols(DesktopState),
    Adapter(DesktopState),
}

impl Composite {
    pub fn new(protocols: Protocols, adapter: Arc<dyn Backend>) -> Self {
        Self { protocols: Arc::new(protocols), adapter }
    }
}

/// The protocols' workspaces and windows with the adapter's other fields. Workspaces pair up
/// by position: the compositor lists them in the same order both ways.
fn merge(protocols: &DesktopState, adapter: &DesktopState) -> DesktopState {
    let mut merged = adapter.clone();
    merged.workspaces = protocols.workspaces.clone();
    for (workspace, known) in merged.workspaces.iter_mut().zip(&adapter.workspaces) {
        workspace.windows = known.windows;
        if workspace.output.is_empty() {
            workspace.output.clone_from(&known.output);
        }
    }
    merged.windows = protocols.windows.clone();
    merged
}

impl Backend for Composite {
    fn watch(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        let (sender, receiver) = mpsc::channel();
        let protocols = self.protocols.clone();
        let to_merge = sender.clone();
        std::thread::spawn(move || {
            let result = protocols.watch(&mut |state| {
                let _ = to_merge.send(Ok(Update::Protocols(state.clone())));
            });
            let _ = to_merge.send(Err(result.err().unwrap_or_else(|| io::Error::other("ended"))));
        });
        let adapter = self.adapter.clone();
        std::thread::spawn(move || {
            let result = adapter.watch(&mut |state| {
                let _ = sender.send(Ok(Update::Adapter(state.clone())));
            });
            let _ = sender.send(Err(result.err().unwrap_or_else(|| io::Error::other("ended"))));
        });

        let mut from_protocols: Option<DesktopState> = None;
        let mut from_adapter: Option<DesktopState> = None;
        let mut last: Option<DesktopState> = None;
        for update in receiver {
            match update? {
                Update::Protocols(state) => from_protocols = Some(state),
                Update::Adapter(state) => from_adapter = Some(state),
            }
            // Until the adapter speaks, the protocols alone already show the desktop.
            let merged = match (&from_protocols, &from_adapter) {
                (Some(p), Some(a)) => merge(p, a),
                (Some(p), None) => p.clone(),
                (None, Some(a)) => a.clone(),
                (None, None) => continue,
            };
            if last.as_ref() != Some(&merged) {
                on_state(&merged);
                last = Some(merged);
            }
        }
        Err(io::Error::other("both connections closed"))
    }

    fn perform(&self, action: &Action) -> io::Result<()> {
        match action {
            Action::FocusWorkspace(_) | Action::FocusWindow(_) | Action::CloseWindow(_) => {
                self.protocols.perform(action)
            }
            // The window switcher is the compositor's own.
            _ => self.adapter.perform(action),
        }
    }

    fn command(&self, method: &str, value: &str) -> io::Result<String> {
        self.adapter.command(method, value)
    }

    fn watch_switcher(&self, on_state: &mut dyn FnMut(&SwitcherState)) -> io::Result<()> {
        self.adapter.watch_switcher(on_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositors::model::{Window, Workspace};

    #[test]
    fn protocols_list_the_adapter_counts() {
        let protocols = DesktopState {
            workspaces: vec![
                Workspace { id: 11, index: 1, active: true, ..Default::default() },
                Workspace { id: 12, index: 2, ..Default::default() },
            ],
            windows: vec![Window { id: 3, title: "Zed".into(), ..Default::default() }],
            ..Default::default()
        };
        let adapter = DesktopState {
            workspaces: vec![
                Workspace {
                    id: 0,
                    index: 1,
                    windows: 2,
                    output: "DP-1".into(),
                    ..Default::default()
                },
                Workspace { id: 1, index: 2, windows: 0, ..Default::default() },
            ],
            windows: vec![Window { id: 99, ..Default::default() }],
            language: "zh_TW".into(),
            ..Default::default()
        };
        let merged = merge(&protocols, &adapter);
        assert_eq!(merged.workspaces[0].id, 11);
        assert_eq!(merged.workspaces[0].windows, 2);
        assert_eq!(merged.workspaces[0].output, "DP-1");
        assert_eq!(merged.windows, protocols.windows);
        assert_eq!(merged.language, "zh_TW");
    }
}
