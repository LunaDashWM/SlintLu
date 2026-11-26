//! Workspaces and windows from standard Wayland protocols, for any compositor that implements
//! them: `ext-workspace-v1` for workspaces and `wlr-foreign-toplevel-management-v1` for windows.
//! Compositor adapters add what the protocols cannot say (which workspace holds a window).

use std::io;
use std::sync::{Arc, Mutex};

use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{wl_registry, wl_seat};
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, EventQueue, QueueHandle, event_created_child,
};
use smithay_client_toolkit::reexports::protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use smithay_client_toolkit::reexports::protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

use super::Backend;
use super::model::{Action, DesktopState, Window, Workspace};

/// One workspace as the compositor describes it.
struct WorkspaceEntry {
    handle: ExtWorkspaceHandleV1,
    /// Our own stable number for it, used in [`Action`]s.
    id: u64,
    name: String,
    coordinates: Vec<u32>,
    state: u32,
}

/// One window (toplevel) as the compositor describes it.
struct ToplevelEntry {
    handle: ZwlrForeignToplevelHandleV1,
    id: u64,
    title: String,
    app_id: String,
    activated: bool,
    /// Announced by a `done` event at least once.
    complete: bool,
}

/// The proxies actions are sent through, shared with [`Backend::perform`].
#[derive(Default)]
struct Handles {
    workspace_manager: Option<ExtWorkspaceManagerV1>,
    workspaces: Vec<(u64, ExtWorkspaceHandleV1)>,
    toplevels: Vec<(u64, ZwlrForeignToplevelHandleV1)>,
    seat: Option<wl_seat::WlSeat>,
}

/// Event dispatch state, owned by [`Protocols::watch`].
struct State {
    handles: Arc<Mutex<Handles>>,
    workspaces: Vec<WorkspaceEntry>,
    toplevels: Vec<ToplevelEntry>,
    next_id: u64,
    /// Something visible changed since the last snapshot.
    dirty: bool,
}

impl State {
    fn next(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn snapshot(&self) -> DesktopState {
        let mut workspaces: Vec<&WorkspaceEntry> = self.workspaces.iter().collect();
        // Compositors that lay workspaces out report coordinates; otherwise creation order.
        workspaces.sort_by(|a, b| a.coordinates.cmp(&b.coordinates).then(a.id.cmp(&b.id)));
        let active = ext_workspace_handle_v1::State::Active.bits();
        let urgent = ext_workspace_handle_v1::State::Urgent.bits();
        let hidden = ext_workspace_handle_v1::State::Hidden.bits();
        let workspaces = workspaces
            .into_iter()
            .filter(|ws| ws.state & hidden == 0)
            .enumerate()
            .map(|(position, ws)| Workspace {
                id: ws.id,
                index: position as i64 + 1,
                name: ws.name.clone(),
                output: String::new(),
                active: ws.state & active != 0,
                focused: ws.state & active != 0,
                urgent: ws.state & urgent != 0,
                windows: 0,
            })
            .collect();
        let windows = self
            .toplevels
            .iter()
            .filter(|t| t.complete)
            .map(|t| Window {
                id: t.id,
                title: t.title.clone(),
                app_id: t.app_id.clone(),
                workspace: None,
                position: None,
                focused: t.activated,
                urgent: false,
            })
            .collect();
        DesktopState { workspaces, windows, ..Default::default() }
    }
}

pub struct Protocols {
    connection: Connection,
    queue: Mutex<Option<EventQueue<State>>>,
    state: Mutex<Option<State>>,
    handles: Arc<Mutex<Handles>>,
}

impl Protocols {
    /// Connects to the compositor; `None` when it offers neither protocol.
    pub fn connect() -> Option<Self> {
        let connection = Connection::connect_to_env().ok()?;
        let (globals, queue) = registry_queue_init::<State>(&connection).ok()?;
        let qh = queue.handle();
        let handles = Arc::new(Mutex::new(Handles::default()));
        let workspace_manager = globals.bind::<ExtWorkspaceManagerV1, _, _>(&qh, 1..=1, ()).ok();
        let toplevel_manager =
            globals.bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 1..=3, ()).ok();
        if workspace_manager.is_none() && toplevel_manager.is_none() {
            return None;
        }
        log::info!(
            "workspaces: standard protocols (ext-workspace {}, wlr-foreign-toplevel {})",
            if workspace_manager.is_some() { "yes" } else { "no" },
            if toplevel_manager.is_some() { "yes" } else { "no" },
        );
        {
            let mut shared = handles.lock().unwrap_or_else(|e| e.into_inner());
            shared.workspace_manager = workspace_manager;
            shared.seat = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 1..=1, ()).ok();
        }
        let state = State {
            handles: handles.clone(),
            workspaces: Vec::new(),
            toplevels: Vec::new(),
            next_id: 0,
            dirty: true,
        };
        Some(Self {
            connection,
            queue: Mutex::new(Some(queue)),
            state: Mutex::new(Some(state)),
            handles,
        })
    }

    /// Whether the compositor reports workspaces (not only windows).
    pub fn has_workspaces(&self) -> bool {
        self.handles.lock().map(|h| h.workspace_manager.is_some()).unwrap_or(false)
    }
}

impl Backend for Protocols {
    fn watch(&self, on_state: &mut dyn FnMut(&DesktopState)) -> io::Result<()> {
        let taken = || io::Error::other("the protocol connection is already being watched");
        let mut queue = self.queue.lock().map_err(|_| taken())?.take().ok_or_else(taken)?;
        let mut state = self.state.lock().map_err(|_| taken())?.take().ok_or_else(taken)?;
        let mut last: Option<DesktopState> = None;
        loop {
            queue.blocking_dispatch(&mut state).map_err(io::Error::other)?;
            if !state.dirty {
                continue;
            }
            state.dirty = false;
            let mut snapshot = state.snapshot();
            snapshot.normalize();
            if last.as_ref() != Some(&snapshot) {
                on_state(&snapshot);
                last = Some(snapshot);
            }
        }
    }

    fn perform(&self, action: &Action) -> io::Result<()> {
        let handles = self.handles.lock().map_err(|_| io::Error::other("poisoned"))?;
        let missing =
            || io::Error::new(io::ErrorKind::NotFound, format!("{action:?}: no such item"));
        match action {
            Action::FocusWorkspace(id) => {
                let manager =
                    handles.workspace_manager.as_ref().ok_or_else(|| super::unsupported(action))?;
                let (_, handle) =
                    handles.workspaces.iter().find(|(i, _)| i == id).ok_or_else(missing)?;
                handle.activate();
                manager.commit();
            }
            Action::FocusWindow(id) => {
                let seat = handles.seat.as_ref().ok_or_else(|| super::unsupported(action))?;
                let (_, handle) =
                    handles.toplevels.iter().find(|(i, _)| i == id).ok_or_else(missing)?;
                handle.activate(seat);
            }
            Action::CloseWindow(id) => {
                let (_, handle) =
                    handles.toplevels.iter().find(|(i, _)| i == id).ok_or_else(missing)?;
                handle.close();
            }
            _ => return Err(super::unsupported(action)),
        }
        self.connection.flush().map_err(io::Error::other)
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                let id = state.next();
                if let Ok(mut handles) = state.handles.lock() {
                    handles.workspaces.push((id, workspace.clone()));
                }
                state.workspaces.push(WorkspaceEntry {
                    handle: workspace,
                    id,
                    name: String::new(),
                    coordinates: Vec::new(),
                    state: 0,
                });
            }
            // Changes come in batches that end with `done`.
            ext_workspace_manager_v1::Event::Done => state.dirty = true,
            ext_workspace_manager_v1::Event::Finished => {
                state.workspaces.clear();
                if let Ok(mut handles) = state.handles.lock() {
                    handles.workspaces.clear();
                    handles.workspace_manager = None;
                }
                state.dirty = true;
            }
            _ => {}
        }
    }

    event_created_child!(State, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtWorkspaceGroupHandleV1,
        _: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_workspace_handle_v1::Event::Removed = event {
            state.workspaces.retain(|ws| &ws.handle != handle);
            if let Ok(mut handles) = state.handles.lock() {
                handles.workspaces.retain(|(_, h)| h != handle);
            }
            handle.destroy();
            return;
        }
        let Some(entry) = state.workspaces.iter_mut().find(|ws| &ws.handle == handle) else {
            return;
        };
        match event {
            ext_workspace_handle_v1::Event::Name { name } => entry.name = name,
            ext_workspace_handle_v1::Event::Coordinates { coordinates } => {
                entry.coordinates =
                    coordinates.as_chunks::<4>().0.iter().map(|b| u32::from_ne_bytes(*b)).collect();
            }
            ext_workspace_handle_v1::Event::State { state: bits } => {
                entry.state = bits.into_result().map(|s| s.bits()).unwrap_or(0);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            let id = state.next();
            if let Ok(mut handles) = state.handles.lock() {
                handles.toplevels.push((id, toplevel.clone()));
            }
            state.toplevels.push(ToplevelEntry {
                handle: toplevel,
                id,
                title: String::new(),
                app_id: String::new(),
                activated: false,
                complete: false,
            });
        }
    }

    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_handle_v1::Event::Closed = event {
            state.toplevels.retain(|t| &t.handle != handle);
            if let Ok(mut handles) = state.handles.lock() {
                handles.toplevels.retain(|(_, h)| h != handle);
            }
            handle.destroy();
            state.dirty = true;
            return;
        }
        let Some(entry) = state.toplevels.iter_mut().find(|t| &t.handle == handle) else {
            return;
        };
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => entry.title = title,
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => entry.app_id = app_id,
            zwlr_foreign_toplevel_handle_v1::Event::State { state: bits } => {
                let activated = zwlr_foreign_toplevel_handle_v1::State::Activated as u32;
                entry.activated =
                    bits.as_chunks::<4>().0.iter().any(|b| u32::from_ne_bytes(*b) == activated);
            }
            // Each batch of changes ends with `done`.
            zwlr_foreign_toplevel_handle_v1::Event::Done => {
                entry.complete = true;
                state.dirty = true;
            }
            _ => {}
        }
    }
}
