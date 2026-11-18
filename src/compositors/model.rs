//! Compositor-independent description of workspaces and windows.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Workspace {
    /// Compositor-specific identifier, used in [`Action`]s.
    pub id: u64,
    /// Position as shown to users (1-based where the compositor numbers workspaces).
    pub index: i64,
    pub name: String,
    /// Connector name of the output showing it.
    pub output: String,
    /// Visible on its output.
    pub active: bool,
    /// Visible on the focused output.
    pub focused: bool,
    pub urgent: bool,
    pub windows: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub workspace: Option<u64>,
    pub focused: bool,
    pub urgent: bool,
}

/// A keyboard shortcut the compositor binds: its action and its keys, e.g. `closeWindow` and
/// `["Meta", "Q"]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shortcut {
    pub action: String,
    pub keys: Vec<String>,
}

/// Everything a shell typically shows about the desktop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopState {
    /// Sorted by output, then index.
    pub workspaces: Vec<Workspace>,
    pub windows: Vec<Window>,
    pub keyboard_layout: String,
    /// The wallpaper image, for compositors that manage it (a local path; empty otherwise).
    pub wallpaper: String,
    /// A still frame of a video wallpaper (a local path; empty for images).
    pub wallpaper_poster: String,
    /// The compositor plays the (video) wallpaper itself, below the shell's surfaces.
    pub wallpaper_live: bool,
    /// The compositor's wallpaper library, for compositors that keep one (LunaDash).
    pub wallpapers: Vec<WallpaperEntry>,
    /// The folder the wallpaper library is read from.
    pub wallpaper_directory: String,
    /// The compositor's own preferences as a JSON object (LunaDash); empty otherwise.
    pub preferences: String,
    /// The desktop language chosen in the compositor, e.g. "zh_TW"; empty when it has none.
    pub language: String,
    /// The compositor is ending the session and asks its shell to exit (LunaDash).
    pub session_ending: bool,
    /// The compositor's keyboard shortcuts, sorted by action, for compositors that report them
    /// (LunaDash). Disabled shortcuts are left out.
    pub shortcuts: Vec<Shortcut>,
}

impl DesktopState {
    pub fn focused_window(&self) -> Option<&Window> {
        self.windows.iter().find(|w| w.focused)
    }

    /// Sorts workspaces and recomputes per-workspace window counts.
    pub fn normalize(&mut self) {
        for ws in &mut self.workspaces {
            ws.windows = self.windows.iter().filter(|w| w.workspace == Some(ws.id)).count() as u32;
        }
        self.workspaces.sort_by(|a, b| a.output.cmp(&b.output).then(a.index.cmp(&b.index)));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    FocusWorkspace(u64),
    FocusWindow(u64),
    CloseWindow(u64),
    /// Starts the window switcher (or the workspace switcher) without a keyboard shortcut.
    OpenSwitcher {
        workspaces: bool,
    },
    /// Moves the switcher's selection by this many entries.
    StepSwitcher(i32),
    /// Selects a switcher entry by id and switches to it.
    ChooseSwitcher(u64),
    /// Closes the switcher; `accept` switches to the selected entry.
    FinishSwitcher {
        accept: bool,
    },
}

/// A wallpaper in the compositor's library.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WallpaperEntry {
    pub path: String,
    pub name: String,
    /// The subfolder it was found in ("Bundled" for the compositor's own).
    pub category: String,
    pub video: bool,
    pub bundled: bool,
    pub favorite: bool,
    /// A still image of it (the file itself for images, a poster frame for videos).
    pub preview: String,
}

/// One entry of the compositor's window switcher.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SwitcherItem {
    /// The window id, or the workspace id in the workspace switcher.
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub workspace: u64,
    pub focused: bool,
    pub minimized: bool,
    /// Workspace switcher: the application ids of the windows on the workspace.
    pub apps: Vec<String>,
}

/// The compositor's window switcher (Alt+Tab), for shells that draw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SwitcherState {
    pub active: bool,
    /// Switching workspaces instead of windows.
    pub workspaces: bool,
    /// The selected entry.
    pub index: usize,
    pub items: Vec<SwitcherItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_counts_and_sorts() {
        let mut state = DesktopState {
            workspaces: vec![
                Workspace { id: 2, index: 2, output: "A".into(), ..Default::default() },
                Workspace { id: 1, index: 1, output: "A".into(), ..Default::default() },
            ],
            windows: vec![
                Window { id: 9, workspace: Some(1), focused: true, ..Default::default() },
                Window { id: 8, workspace: Some(1), ..Default::default() },
            ],
            ..Default::default()
        };
        state.normalize();
        assert_eq!(state.workspaces[0].id, 1);
        assert_eq!(state.workspaces[0].windows, 2);
        assert_eq!(state.workspaces[1].windows, 0);
        assert_eq!(state.focused_window().map(|w| w.id), Some(9));
    }
}
