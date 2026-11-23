//! Desktop shell model: the sorted Start-menu catalog and open-window registry.

use crate::voice;

/// A built-in GUI function that can be opened as a desktop window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopApp {
    /// Live system metrics and process controls.
    Dashboard,
    /// Loaded WASM blocks and hot-swap actions.
    Blocks,
    /// Streaming LLM chat.
    AiStudio,
    /// Local block marketplace.
    Store,
    /// Network configuration form.
    Network,
    /// Block dependency graph.
    Deps,
    /// Native WebView browser.
    Browser,
    /// Two-panel virtual file manager.
    Files,
    /// Hardware and driver provisioning.
    Hardware,
}

impl DesktopApp {
    /// Human-readable name shown in the Start menu, taskbar and window title.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "System Dashboard",
            Self::Blocks => "WASM Blocks",
            Self::AiStudio => "AI Studio",
            Self::Store => "App Store",
            Self::Network => "Network Settings",
            Self::Deps => "Dependencies",
            Self::Browser => "Native Browser",
            Self::Files => "Files",
            Self::Hardware => "Hardware & Drivers",
        }
    }

    /// Keyboard shortcut that opens this function.
    pub const fn shortcut(self) -> &'static str {
        match self {
            Self::Dashboard => "F1",
            Self::Blocks => "F2",
            Self::AiStudio => "F3",
            Self::Store => "F4",
            Self::Network => "F5",
            Self::Deps => "F6",
            Self::Browser => "F7",
            Self::Files => "F8",
            Self::Hardware => "F9",
        }
    }

    /// All built-in functions available in this build.
    pub fn all() -> Vec<Self> {
        let mut apps = vec![
            Self::Dashboard,
            Self::Blocks,
            Self::AiStudio,
            Self::Store,
            Self::Network,
            Self::Deps,
        ];
        #[cfg(feature = "webview")]
        apps.push(Self::Browser);
        apps.extend([Self::Files, Self::Hardware]);
        apps
    }

    /// Resolve an `F1`-`F9` key press to a built-in function.
    pub fn from_key(key: egui::Key) -> Option<Self> {
        match key {
            egui::Key::F1 => Some(Self::Dashboard),
            egui::Key::F2 => Some(Self::Blocks),
            egui::Key::F3 => Some(Self::AiStudio),
            egui::Key::F4 => Some(Self::Store),
            egui::Key::F5 => Some(Self::Network),
            egui::Key::F6 => Some(Self::Deps),
            egui::Key::F7 => Some(Self::Browser),
            egui::Key::F8 => Some(Self::Files),
            egui::Key::F9 => Some(Self::Hardware),
            _ => None,
        }
    }
}

/// What happens when a Start-menu entry is activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartAction {
    /// Open or focus a built-in function window.
    Open(DesktopApp),
    /// Launch a shared catalog program.
    RunProgram(&'static str),
    /// Refresh every live data source.
    RefreshAll,
    /// Suspend every running system process.
    SuspendAll,
}

/// One sorted entry in the Start menu.
#[derive(Debug, Clone)]
pub struct StartEntry {
    /// Display name.
    pub title: String,
    /// Short hint shown under the name.
    pub subtitle: String,
    /// Action to execute.
    pub action: StartAction,
}

/// Build the full Start-menu catalog, sorted alphabetically by display name.
pub fn start_entries() -> Vec<StartEntry> {
    let mut entries: Vec<StartEntry> = DesktopApp::all()
        .into_iter()
        .map(|app| StartEntry {
            title: app.title().to_string(),
            subtitle: format!("{} · AIOS", app.shortcut()),
            action: StartAction::Open(app),
        })
        .collect();

    entries.extend(voice::programs().iter().map(|program| StartEntry {
        title: program.title.to_string(),
        subtitle: "Программа".to_string(),
        action: StartAction::RunProgram(program.title),
    }));

    entries.push(StartEntry {
        title: "Обновить всё".to_string(),
        subtitle: "Система".to_string(),
        action: StartAction::RefreshAll,
    });
    entries.push(StartEntry {
        title: "Приостановить всё".to_string(),
        subtitle: "Система".to_string(),
        action: StartAction::SuspendAll,
    });

    entries.sort_by_key(|entry| entry.title.to_lowercase());
    entries
}

/// State of one open desktop window.
#[derive(Debug, Clone)]
pub struct DesktopWindow {
    /// Stable identifier used by the taskbar and focus tracking.
    pub id: u64,
    /// Function rendered inside the window.
    pub app: DesktopApp,
    /// Whether the window is still open.
    pub open: bool,
    /// Whether the window is minimized to the taskbar.
    pub minimized: bool,
}

/// Mutable state of the desktop shell.
#[derive(Debug, Default)]
pub struct DesktopState {
    /// Currently open windows in opening order.
    pub windows: Vec<DesktopWindow>,
    /// Window that receives global keyboard navigation.
    pub focused: Option<u64>,
    /// Monotonic id source for new windows.
    pub next_id: u64,
    /// Whether the Start menu is visible.
    pub start_open: bool,
    /// Start-menu search filter.
    pub start_query: String,
}

impl DesktopState {
    /// Open a function window or restore an existing one.
    pub fn open(&mut self, app: DesktopApp) {
        if let Some(window) = self.windows.iter_mut().find(|w| w.app == app) {
            window.open = true;
            window.minimized = false;
            self.focused = Some(window.id);
            self.start_open = false;
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.windows.push(DesktopWindow {
            id,
            app,
            open: true,
            minimized: false,
        });
        self.focused = Some(id);
        self.start_open = false;
    }

    /// Open or close the Start menu.
    pub fn toggle_start(&mut self) {
        self.start_open = !self.start_open;
    }

    /// Close a window and remove it from the taskbar.
    pub fn close(&mut self, id: u64) {
        self.windows.retain(|w| w.id != id);
        if self.focused == Some(id) {
            self.focused = self.windows.last().map(|w| w.id);
        }
    }

    /// Focus a window, restoring it when minimized.
    pub fn focus(&mut self, id: u64) {
        if let Some(window) = self.windows.iter_mut().find(|w| w.id == id) {
            window.minimized = false;
            self.focused = Some(id);
        }
    }

    /// Toggle minimization of a window from the taskbar.
    pub fn toggle_minimize(&mut self, id: u64) {
        let is_focused = self.focused == Some(id);
        if let Some(window) = self.windows.iter_mut().find(|w| w.id == id) {
            if is_focused && !window.minimized {
                window.minimized = true;
                self.focused = None;
            } else {
                window.minimized = false;
                self.focused = Some(id);
            }
        }
    }

    /// Function rendered by the focused window, if any.
    pub fn focused_app(&self) -> Option<DesktopApp> {
        self.focused
            .and_then(|id| self.windows.iter().find(|w| w.id == id))
            .filter(|w| w.open && !w.minimized)
            .map(|w| w.app)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_entries_are_sorted_and_unique() {
        let entries = start_entries();
        assert!(entries.len() > DesktopApp::all().len());
        let keys: Vec<String> = entries.iter().map(|e| e.title.to_lowercase()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        let mut unique = keys.clone();
        unique.dedup();
        assert_eq!(keys.len(), unique.len());
    }

    #[test]
    fn start_entries_cover_every_builtin_and_program() {
        let entries = start_entries();
        for app in DesktopApp::all() {
            assert!(entries.iter().any(|e| e.title == app.title()));
        }
        for program in voice::programs() {
            assert!(entries.iter().any(|e| e.title == program.title));
        }
    }

    #[test]
    fn desktop_window_focus_and_minimize() {
        let mut state = DesktopState::default();
        state.open(DesktopApp::Files);
        state.open(DesktopApp::Blocks);
        assert_eq!(state.focused_app(), Some(DesktopApp::Blocks));

        let files_id = state.windows[0].id;
        state.focus(files_id);
        assert_eq!(state.focused_app(), Some(DesktopApp::Files));

        state.toggle_minimize(files_id);
        assert_eq!(state.focused_app(), None);
        state.toggle_minimize(files_id);
        assert_eq!(state.focused_app(), Some(DesktopApp::Files));

        state.close(files_id);
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.focused_app(), Some(DesktopApp::Blocks));
    }

    #[cfg(not(feature = "webview"))]
    #[test]
    fn browser_is_absent_without_webview() {
        assert!(!DesktopApp::all().contains(&DesktopApp::Browser));
    }
}
