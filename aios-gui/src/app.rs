use crate::tabs;
use crate::theme::AiosTheme;

use aios_fm::commands::{Ack, Command};
use aios_fm::engine::FileManager;
use aios_fm::state::PanelSide;
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_net_config::config::NetworkConfig;
use aios_vfs::ai_preview::AiPreview;
use aios_vfs::security::AclContext;
use aios_vfs::vfs::{AiosVfs, VirtualFileSystem};
use tokio::sync::mpsc::UnboundedReceiver;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One persisted chat entry of the AI Studio (same JSONL schema as the TUI).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiMessage {
    pub role: String,
    pub text: String,
}

/// Default data directory used when `AIOS_DATA_DIR` is not set.
fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("AIOS_DATA_DIR").unwrap_or_else(|_| "aios_data".into()))
}

fn chat_path() -> PathBuf {
    data_dir().join("chat.jsonl")
}

fn presets_path() -> PathBuf {
    data_dir().join("presets.json")
}

fn seed_presets() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("assistant".into(), "You are a helpful AI assistant.".into());
    m.insert(
        "code".into(),
        "You are an expert senior software engineer. Give concise, idiomatic \
         code with brief explanations. Prefer standard library solutions."
            .into(),
    );
    m.insert(
        "translator".into(),
        "You translate text between languages accurately, preserving meaning \
         and tone. Output only the translation."
            .into(),
    );
    m.insert(
        "explainer".into(),
        "You explain complex topics in simple terms with concrete examples.".into(),
    );
    m
}

/// Create the aios-autohal engine for a hardware snapshot and run the initial
/// provisioning pass so the Hardware & Drivers tab has data on first render.
fn init_hw_engine(
    hardware: &HardwareProfile,
) -> (
    Option<aios_autohal::AutohalEngine>,
    Vec<aios_autohal::DeviceView>,
    Vec<aios_autohal::Toast>,
) {
    let Ok(mut engine) = aios_autohal::AutohalEngine::new(aios_autohal::EngineConfig::default())
    else {
        log::warn!("AUTOHAL: engine init failed");
        return (None, Vec::new(), Vec::new());
    };
    engine.rescan(hardware);
    let toasts = engine.pop_toasts(16);
    let views = engine.device_views();
    (Some(engine), views, toasts)
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u64,
    pub name: String,
    pub priority: String,
    pub state: String,
    pub ram_mb: u64,
    pub cpu_ms: u64,
    pub crashes: u32,
}

#[derive(Debug, Clone)]
pub struct BlockInfo {
    pub id: u32,
    pub name: String,
    pub version: String,
    pub state: String,
    pub size: usize,
    pub deps: Vec<String>,
    pub dependents: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MarketplaceEntry {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub status: String,
    pub tags: Vec<String>,
    pub downloads: u64,
}

pub struct AiosApp {
    pub ai_tier: AiTier,
    pub hardware: HardwareProfile,
    pub ram_used: u64,
    pub ram_total: u64,
    pub ram_history: Vec<f32>,
    pub processes: Vec<ProcessInfo>,
    pub blocks: Vec<BlockInfo>,
    pub watchdog_state: u8,
    pub log_messages: Vec<String>,

    pub selected_tab: usize,
    pub selected_process_idx: Option<usize>,
    pub selected_block_idx: Option<usize>,
    pub selected_marketplace_idx: Option<usize>,

    pub show_load_dialog: bool,
    pub load_name_buf: String,
    pub load_version_buf: String,
    pub load_step: u8,

    pub marketplace_search: String,
    pub marketplace_entries: Vec<MarketplaceEntry>,
    pub marketplace_status: Option<String>,

    pub dep_blocks: Vec<String>,
    pub dep_load_order: Vec<String>,
    pub dep_edges: Vec<(String, String)>,

    pub browser: Option<aios_webview::WebBrowser>,
    pub browser_addr: String,
    pub browser_status: Option<String>,
    /// True while a background thread is still creating the native window, so
    /// repeated clicks do not spawn a second browser.
    pub browser_opening: bool,
    pending_browser: Arc<Mutex<Option<aios_webview::WebBrowser>>>,
    pending_browser_error: Arc<Mutex<Option<String>>>,

    pub uptime_secs: u64,

    pub ai_config: aios_llm::LlmConfig,
    pub ai_input: String,
    pub ai_output: Vec<String>,
    pub ai_busy: bool,
    pub ai_status: String,
    pub ai_system_prompt: String,
    pub ai_presets: BTreeMap<String, String>,
    pub ai_log: Vec<AiMessage>,
    pub ai_stream: Arc<Mutex<String>>,
    pending_ai: Arc<Mutex<Option<Result<String, String>>>>,

    pub ipc_traffic: u64,

    pub net_config: NetworkConfig,
    pub net_status: Option<String>,

    /// File-manager engine (created with a dedicated tokio runtime).
    pub fm: Option<FileManager>,
    /// Runtime hosting the FM engine loop and background jobs.
    pub fm_rt: Option<tokio::runtime::Runtime>,
    /// Outbox for FM job acknowledgements.
    pub fm_ack: Option<UnboundedReceiver<Ack>>,
    /// AI preview of the currently viewed file (Files tab).
    pub fm_preview: Option<AiPreview>,
    /// Error message from the last FM operation, if any.
    pub fm_error: Option<String>,
    /// Modal input (mkdir / rename) state for the Files tab.
    pub fm_input: Option<FmInput>,
    /// Buffer for the active Files-tab modal input.
    pub fm_input_buf: String,

    /// Hardware auto-provisioning engine (aios-autohal), created at startup.
    pub hw_engine: Option<aios_autohal::AutohalEngine>,
    /// Latest `DeviceView` snapshots for the Hardware & Drivers tab (F9).
    pub hw_views: Vec<aios_autohal::DeviceView>,
    /// Recent provisioning toasts (hot-plug strip).
    pub hw_toasts: Vec<aios_autohal::Toast>,
    /// Live device hot-plug monitor, started alongside the engine.
    pub hw_hotplug: Option<aios_autohal::HotplugMonitor>,
}

/// Active modal input mode of the GUI Files tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FmInput {
    Mkdir,
    Rename,
}

impl AiosApp {
    pub fn new(
        ai_tier: AiTier,
        hardware: HardwareProfile,
        dep_blocks: Vec<String>,
        dep_load_order: Vec<String>,
        dep_edges: Vec<(String, String)>,
        block_count: usize,
        ram_total: u64,
    ) -> Self {
        let hw_init = init_hw_engine(&hardware);
        let hw_hotplug = if hw_init.0.is_some() {
            Some(aios_autohal::HotplugMonitor::start(Default::default()))
        } else {
            None
        };
        Self {
            ai_tier,
            hardware,
            ram_used: 0,
            ram_total,
            ram_history: vec![0.0],
            processes: Vec::new(),
            blocks: Vec::new(),
            watchdog_state: 0,
            log_messages: vec![
                "System initialized".into(),
                format!("AI Tier: {}", ai_tier),
                format!("{block_count} blocks loaded"),
            ],
            selected_tab: 0,
            selected_process_idx: None,
            selected_block_idx: None,
            selected_marketplace_idx: None,
            show_load_dialog: false,
            load_name_buf: String::new(),
            load_version_buf: String::new(),
            load_step: 0,
            marketplace_search: String::new(),
            marketplace_entries: Vec::new(),
            marketplace_status: None,
            dep_blocks,
            dep_load_order,
            dep_edges,
            browser: None,
            browser_addr: String::new(),
            browser_status: None,
            browser_opening: false,
            pending_browser: Arc::new(Mutex::new(None)),
            pending_browser_error: Arc::new(Mutex::new(None)),
            uptime_secs: 0,
            ai_config: aios_llm::default_config(),
            ai_input: String::new(),
            ai_output: Vec::new(),
            ai_busy: false,
            ai_status: "ready".into(),
            ai_system_prompt: "You are a helpful AI assistant.".into(),
            ai_presets: seed_presets(),
            ai_log: Vec::new(),
            ai_stream: Arc::new(Mutex::new(String::new())),
            pending_ai: Arc::new(Mutex::new(None)),
            ipc_traffic: 0,
            net_config: NetworkConfig::default(),
            net_status: None,
            fm: None,
            fm_rt: None,
            fm_ack: None,
            fm_preview: None,
            fm_error: None,
            fm_input: None,
            fm_input_buf: String::new(),
            hw_engine: hw_init.0,
            hw_views: hw_init.1,
            hw_toasts: hw_init.2,
            hw_hotplug,
        }
    }

    /// Start the file-manager engine on a dedicated tokio runtime.
    pub fn fm_init(&mut self) {
        let fm_root = data_dir().join("vfs_sandbox");
        match tokio::runtime::Runtime::new() {
            Ok(rt) => {
                let (fm, fm_ack) = {
                    let vfs: Arc<dyn VirtualFileSystem> =
                        Arc::new(AiosVfs::new(fm_root.clone()).expect("VFS root init"));
                    rt.block_on(async move { FileManager::new(vfs, Arc::new(AclContext::new())) })
                };
                self.fm = Some(fm);
                self.fm_rt = Some(rt);
                self.fm_ack = Some(fm_ack);
                self.add_log(format!("File manager started on {}", fm_root.display()));
            }
            Err(e) => {
                self.fm_error = Some(format!("FM runtime failed: {e}"));
                self.add_log(format!("FM runtime failed: {e}"));
            }
        }
    }

    /// Drain pending FM acknowledgements, updating the AI preview and log.
    pub fn poll_fm_acks(&mut self) {
        loop {
            let ack = match self.fm_ack.as_mut() {
                Some(rx) => match rx.try_recv() {
                    Ok(ack) => ack,
                    Err(_) => return,
                },
                None => return,
            };
            match ack {
                Ack::DirChanged { side } => self.add_log(format!("FM: {} refreshed", side.name())),
                Ack::Copied(s) => {
                    self.add_log(format!("FM: copied {} files ({} B)", s.files, s.bytes));
                }
                Ack::Moved(s) => self.add_log(format!("FM: moved {} files", s.files)),
                Ack::Deleted(s) => {
                    self.add_log(format!("FM: deleted {} files ({} B)", s.files, s.bytes));
                }
                Ack::CreatedDir { path } => {
                    self.add_log(format!("FM: created {}", path.to_uri()));
                }
                Ack::Renamed { from, to } => {
                    self.add_log(format!("FM: renamed {} -> {}", from.to_uri(), to.to_uri()));
                }
                Ack::View { preview, .. } => {
                    self.fm_preview = Some(preview);
                    self.add_log("FM: AI preview ready".into());
                }
                Ack::Error(e) => {
                    self.fm_error = Some(e.clone());
                    self.add_log(format!("FM: error: {e}"));
                }
            }
        }
    }

    /// Execute a file-manager action on the active panel.
    pub fn fm_act(&mut self, action: aios_fm::ui_tui::TuiAction) {
        let Some(fm) = self.fm.clone() else { return };
        match action {
            aios_fm::ui_tui::TuiAction::MoveUp { side } => fm.move_cursor(side, -1),
            aios_fm::ui_tui::TuiAction::MoveDown { side } => fm.move_cursor(side, 1),
            aios_fm::ui_tui::TuiAction::Enter { side } => {
                if fm.selected_is_dir(side) == Some(true) {
                    if let Some(path) = fm.selected(side) {
                        fm.send(Command::Navigate { side, path });
                    }
                } else if let Some(path) = fm.selected(side) {
                    fm.send(Command::View { path });
                }
            }
            aios_fm::ui_tui::TuiAction::GoUp { side } => {
                let parent = fm.panel_path(side).parent();
                fm.send(Command::Navigate { side, path: parent });
            }
            aios_fm::ui_tui::TuiAction::SwitchPanel => fm.switch_panel(),
            aios_fm::ui_tui::TuiAction::CopySelected => {
                let side = fm.active_side();
                if let (Some(src), Some(dst)) = (fm.selected(side), fm.default_target(side)) {
                    fm.send(Command::Copy { src, dst });
                    self.add_log("FM: copying...".into());
                }
            }
            aios_fm::ui_tui::TuiAction::MoveSelected => {
                let side = fm.active_side();
                if let (Some(src), Some(dst)) = (fm.selected(side), fm.default_target(side)) {
                    fm.send(Command::Move { src, dst });
                    self.add_log("FM: moving...".into());
                }
            }
            aios_fm::ui_tui::TuiAction::DeleteSelected => {
                let side = fm.active_side();
                if let Some(path) = fm.selected(side) {
                    fm.send(Command::Delete { path });
                    self.add_log("FM: deleting...".into());
                }
            }
            aios_fm::ui_tui::TuiAction::Mkdir { .. } => {
                self.fm_input = Some(FmInput::Mkdir);
                self.fm_input_buf.clear();
            }
            aios_fm::ui_tui::TuiAction::Rename { .. } => {
                let side = fm.active_side();
                if fm.selected(side).is_some() {
                    self.fm_input = Some(FmInput::Rename);
                    self.fm_input_buf.clear();
                }
            }
            aios_fm::ui_tui::TuiAction::ViewSelected => {
                let side = fm.active_side();
                if let Some(path) = fm.selected(side) {
                    fm.send(Command::View { path });
                    self.add_log("FM: AI preview...".into());
                }
            }
            aios_fm::ui_tui::TuiAction::ToggleSort { side } => fm.toggle_sort(side),
            aios_fm::ui_tui::TuiAction::GrantHostRead => {
                fm.send(Command::GrantHostRead);
                self.add_log("FM: granted vfs:host:read".into());
            }
            aios_fm::ui_tui::TuiAction::GrantHostWrite => {
                fm.send(Command::GrantHostWrite);
                self.add_log("FM: granted vfs:host:write".into());
            }
            aios_fm::ui_tui::TuiAction::Refresh { side } => fm.send(Command::Refresh { side }),
            aios_fm::ui_tui::TuiAction::Close => {
                self.fm_preview = None;
            }
        }
    }

    /// Refresh Hardware & Drivers tab data from the engine (views + toasts).
    pub fn hw_refresh(&mut self) {
        let Some(engine) = &mut self.hw_engine else {
            return;
        };
        self.hw_views = engine.device_views();
        let fresh = engine.pop_toasts(10);
        if !fresh.is_empty() {
            self.hw_toasts.extend(fresh);
            if self.hw_toasts.len() > 24 {
                let excess = self.hw_toasts.len() - 24;
                self.hw_toasts.drain(0..excess);
            }
        }
    }

    /// Apply actions emitted by the Hardware & Drivers panel (F9).
    pub fn apply_hw_actions(&mut self, actions: Vec<aios_autohal::ui_gui::GuiAction>) {
        let Some(engine) = &mut self.hw_engine else {
            return;
        };
        for action in actions {
            match action {
                aios_autohal::ui_gui::GuiAction::Rescan => {
                    engine.rescan(&self.hardware);
                }
                aios_autohal::ui_gui::GuiAction::Update { index } => {
                    if let Some(dev) = self.hw_views.get(index) {
                        engine.provision_blocking(dev.fingerprint.clone());
                    }
                }
                aios_autohal::ui_gui::GuiAction::Rollback { index } => {
                    if let Some(dev) = self.hw_views.get(index) {
                        engine.rollback_to_generic(&dev.fingerprint);
                    }
                }
                aios_autohal::ui_gui::GuiAction::Uninstall { index } => {
                    if let Some(dev) = self.hw_views.get(index) {
                        engine.uninstall_driver(&dev.driver_id);
                    }
                }
                aios_autohal::ui_gui::GuiAction::SetCapabilities { index, caps } => {
                    if let Some(dev) = self.hw_views.get(index) {
                        engine.set_cap_override(&dev.driver_id, caps);
                    }
                }
            }
        }
    }

    /// Drain hot-plug events from the background monitor and apply them to the
    /// engine (provision on arrival, unload on removal). Cheap when idle.
    pub fn hw_poll_hotplug(&mut self) {
        let events = match &self.hw_hotplug {
            Some(monitor) => monitor.drain(),
            None => return,
        };
        if events.is_empty() {
            return;
        }
        let Some(engine) = &mut self.hw_engine else {
            return;
        };
        for event in events {
            match event {
                aios_autohal::HotplugEvent::Added(fp) => engine.provision_blocking(fp),
                aios_autohal::HotplugEvent::Removed(fp) => engine.remove_device(&fp),
            }
        }
        self.hw_refresh();
    }

    /// Confirm the active Files-tab modal input (mkdir / rename).
    pub fn fm_confirm_input(&mut self) {
        let name = self.fm_input_buf.trim().to_string();
        let mode = self.fm_input.take();
        self.fm_input_buf.clear();
        if name.is_empty() {
            return;
        }
        let Some(fm) = self.fm.clone() else { return };
        let side = fm.active_side();
        match mode {
            Some(FmInput::Mkdir) => fm.send(Command::Mkdir {
                side,
                parent: fm.panel_path(side),
                name,
            }),
            Some(FmInput::Rename) => {
                if let Some(from) = fm.selected(side) {
                    let to = from.parent().join(&name);
                    fm.send(Command::Rename { side, from, to });
                }
            }
            None => {}
        }
    }

    /// Map a GUI key to a file-manager action on the given panel side.
    fn fm_key_action(k: egui::Key, side: PanelSide) -> Option<aios_fm::ui_tui::TuiAction> {
        use aios_fm::ui_tui::TuiAction;
        match k {
            egui::Key::ArrowUp => Some(TuiAction::MoveUp { side }),
            egui::Key::ArrowDown => Some(TuiAction::MoveDown { side }),
            egui::Key::Tab => Some(TuiAction::SwitchPanel),
            egui::Key::Enter => Some(TuiAction::Enter { side }),
            egui::Key::Backspace => Some(TuiAction::GoUp { side }),
            egui::Key::F3 => Some(TuiAction::ViewSelected),
            egui::Key::F5 => Some(TuiAction::CopySelected),
            egui::Key::F6 => Some(TuiAction::MoveSelected),
            egui::Key::F7 => Some(TuiAction::Mkdir { side }),
            egui::Key::F8 => Some(TuiAction::DeleteSelected),
            egui::Key::F9 => Some(TuiAction::ToggleSort { side }),
            egui::Key::R => Some(TuiAction::Refresh { side }),
            egui::Key::G => Some(TuiAction::GrantHostRead),
            egui::Key::W => Some(TuiAction::GrantHostWrite),
            egui::Key::Escape => Some(TuiAction::Close),
            _ => None,
        }
    }

    pub fn add_log(&mut self, msg: String) {
        self.log_messages.push(msg);
        if self.log_messages.len() > 200 {
            self.log_messages.remove(0);
        }
    }

    pub fn refresh_processes(&mut self) {
        self.add_log("Refreshed process list".into());
    }

    pub fn refresh_blocks(&mut self) {
        self.add_log("Refreshed block list".into());
    }

    pub fn kill_process(&mut self, pid: u64) {
        self.add_log(format!("Kill process PID {pid}"));
    }

    pub fn suspend_process(&mut self, pid: u64) {
        self.add_log(format!("Suspend process PID {pid}"));
    }

    pub fn resume_process(&mut self, pid: u64) {
        self.add_log(format!("Resume process PID {pid}"));
    }

    pub fn load_block(&mut self, name: String, version: String) {
        self.add_log(format!("Loading block {name} v{version}"));
    }

    pub fn unload_block(&mut self, id: u32) {
        self.add_log(format!("Unloading block ID {id}"));
    }

    pub fn search_marketplace(&mut self) {
        if self.marketplace_search.is_empty() {
            self.marketplace_status = None;
        } else {
            let q = self.marketplace_search.to_lowercase();
            let count = self
                .marketplace_entries
                .iter()
                .filter(|e| {
                    e.name.to_lowercase().contains(&q)
                        || e.description.to_lowercase().contains(&q)
                        || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
                })
                .count();
            self.marketplace_status = Some(format!("Found {count} matching blocks"));
        }
    }

    pub fn install_block(&mut self, name: String) {
        self.add_log(format!("Installing block: {name}"));
        self.marketplace_status = Some(format!("Installing {name}..."));
    }

    pub fn update_block(&mut self, name: String) {
        self.add_log(format!("Updating block: {name}"));
        self.marketplace_status = Some(format!("Updating {name}..."));
    }

    pub fn uninstall_block(&mut self, name: String) {
        self.add_log(format!("Uninstalling block: {name}"));
        self.marketplace_status = Some(format!("Uninstalled {name}"));
    }

    pub fn browser_active(&self) -> bool {
        self.browser.is_some()
    }

    /// Start opening the native browser on a background thread. The UI stays
    /// responsive and repeated calls are ignored while an open is in flight.
    fn start_browser_open(&mut self, target: String) {
        if self.browser.is_some() || self.browser_opening {
            return;
        }
        self.browser_opening = true;
        if let Ok(mut slot) = self.pending_browser.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = self.pending_browser_error.lock() {
            *slot = None;
        }
        self.browser_status = Some(format!("Opening browser: {target}"));
        self.add_log(format!("Browser opening: {target}"));

        let slot = self.pending_browser.clone();
        let err_slot = self.pending_browser_error.clone();
        std::thread::spawn(move || match aios_webview::WebBrowser::open(&target) {
            Ok(browser) => {
                if let Ok(mut guard) = slot.lock() {
                    *guard = Some(browser);
                }
            }
            Err(e) => {
                if let Ok(mut guard) = err_slot.lock() {
                    *guard = Some(e);
                }
            }
        });
    }

    /// Pick up the result of a background browser open, if it has finished.
    fn poll_browser_open(&mut self) {
        if !self.browser_opening {
            return;
        }
        let err = self
            .pending_browser_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(e) = err {
            self.browser_opening = false;
            self.browser_status = Some(format!("Failed to open browser: {e}"));
            self.add_log(format!("Browser failed to open: {e}"));
            return;
        }
        let got = {
            let mut slot = self
                .pending_browser
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            slot.take()
        };
        if let Some(browser) = got {
            self.browser_opening = false;
            self.browser = Some(browser);
            self.browser_status = Some("Browser opened".into());
            self.add_log("Browser opened".into());
        }
    }

    pub fn open_browser(&mut self) -> Result<(), String> {
        if self.browser.is_some() {
            return Ok(());
        }
        let target = aios_webview::resolve_target(self.browser_addr.trim());
        self.start_browser_open(target);
        Ok(())
    }

    pub fn navigate_browser(&mut self, input: &str) -> Result<(), String> {
        let target = aios_webview::resolve_target(input);
        match self.browser.as_ref() {
            Some(browser) => {
                browser.navigate(&target)?;
                self.browser_status = Some(format!("Navigate: {target}"));
            }
            None => {
                self.add_log(format!("Browser -> {target}"));
                self.start_browser_open(target);
            }
        }
        Ok(())
    }

    pub fn browser_back(&mut self) -> Result<(), String> {
        match self.browser.as_ref() {
            Some(browser) => {
                browser.back()?;
                self.browser_status = Some("History: back".into());
                Ok(())
            }
            None => Err("Browser is not open".into()),
        }
    }

    pub fn browser_forward(&mut self) -> Result<(), String> {
        match self.browser.as_ref() {
            Some(browser) => {
                browser.forward()?;
                self.browser_status = Some("History: forward".into());
                Ok(())
            }
            None => Err("Browser is not open".into()),
        }
    }

    pub fn close_browser(&mut self) {
        self.browser_opening = false;
        if let Ok(mut slot) = self.pending_browser.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = self.pending_browser_error.lock() {
            *slot = None;
        }
        if self.browser.is_some() {
            self.browser = None;
            self.browser_status = Some("Browser closed".into());
            self.add_log("Browser closed".into());
        }
    }

    /// Send the current AI input as a query (or a `/command`) on a background
    /// thread using its own tokio runtime; the UI stays responsive and the
    /// answer streams into `ai_stream` token-by-token.
    pub fn ai_send(&mut self) {
        let input = self.ai_input.trim().to_string();
        if input.is_empty() || self.ai_busy {
            return;
        }
        self.ai_input.clear();
        if let Some(cmd) = input.strip_prefix('/') {
            self.ai_command(cmd);
            return;
        }
        self.ai_output.push(format!("> {input}"));
        self.ai_log.push(AiMessage {
            role: "user".into(),
            text: input.clone(),
        });
        let config = self.ai_config.clone();
        let system = self.ai_system_prompt.clone();
        let slot = self.pending_ai.clone();
        let stream = self.ai_stream.clone();
        self.ai_busy = true;
        self.ai_status = "streaming...".into();
        if let Ok(mut s) = stream.lock() {
            s.clear();
        }
        std::thread::spawn(move || {
            let result = match tokio::runtime::Runtime::new() {
                Ok(rt) => {
                    let engine = aios_llm::LlmEngine::from_config(config.clone());
                    let req = aios_llm::LlmRequest {
                        system_prompt: system,
                        user_prompt: input,
                        max_tokens: config.max_tokens,
                        temperature: config.temperature,
                    };
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    rt.spawn(async move { engine.query_stream(&req, tx).await });
                    let mut full = String::new();
                    let mut error: Option<String> = None;
                    while let Some(item) = rx.blocking_recv() {
                        match item {
                            Ok(delta) => {
                                full.push_str(&delta);
                                if let Ok(mut s) = stream.lock() {
                                    s.push_str(&delta);
                                }
                            }
                            Err(e) => {
                                error = Some(e.to_string());
                                break;
                            }
                        }
                    }
                    match error {
                        Some(e) => Err(e),
                        None => Ok(full),
                    }
                }
                Err(e) => Err(format!("runtime init failed: {e}")),
            };
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(result);
            }
        });
    }

    /// Pick up the result of a background AI query, if it has finished.
    pub fn poll_ai(&mut self) {
        let got = {
            let mut slot = self.pending_ai.lock().unwrap_or_else(|p| p.into_inner());
            slot.take()
        };
        if let Some(result) = got {
            self.ai_busy = false;
            let tail = {
                let mut s = self.ai_stream.lock().unwrap();
                let tail = s.clone();
                s.clear();
                tail
            };
            match result {
                Ok(text) => {
                    let chars = text.len();
                    if !tail.trim().is_empty() {
                        self.ai_output.push(tail.clone());
                        self.ai_log.push(AiMessage {
                            role: "assistant".into(),
                            text: tail,
                        });
                        self.ai_save_chat();
                    }
                    self.ai_status = "ready".into();
                    self.add_log(format!("AI: query returned {chars} chars"));
                }
                Err(e) => {
                    if !tail.trim().is_empty() {
                        self.ai_output.push(tail);
                    }
                    self.ai_output.push(format!("[error] {e}"));
                    self.ai_status = "error".into();
                    self.add_log(format!("AI: query failed: {e}"));
                }
            }
        }
    }

    /// Handle a slash command locally (same grammar as the TUI AI Console).
    pub fn ai_command(&mut self, cmd: &str) {
        let mut parts = cmd.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match name {
            "help" => {
                for l in [
                    "/help            open this panel",
                    "/status          show backend, model and parameter info",
                    "/clear           clear the chat output",
                    "/history         list the recent prompts",
                    "/system <text>   set the system prompt",
                    "/model <name>    set the model",
                    "/backend <kind>  groq | openrouter | google | micro | full",
                    "/key <api-key>   set the API key (no argument clears it)",
                    "/temp <0.0-2.0>  set sampling temperature",
                    "/tokens <1-8192> set max output tokens",
                    "/preset <name>   apply a prompt template",
                    "/preset <name> <text>  save a template | list | del <name>",
                    "/save            persist the chat to disk",
                    "/load            restore the chat from disk",
                ] {
                    self.ai_output.push(format!("  {l}"));
                }
            }
            "clear" => self.ai_output.clear(),
            "history" => {
                if self.ai_log.is_empty() {
                    self.ai_output.push("  history is empty".into());
                } else {
                    for msg in self.ai_log.iter().filter(|m| m.role == "user") {
                        self.ai_output.push(format!("  > {}", msg.text));
                    }
                }
            }
            "system" => {
                if arg.is_empty() {
                    self.ai_output
                        .push(format!("  system prompt: {}", self.ai_system_prompt));
                } else {
                    self.ai_system_prompt = arg.to_string();
                    self.ai_output.push("  system prompt updated".into());
                }
            }
            "preset" => {
                let (pname, ptext) = match arg.split_once(char::is_whitespace) {
                    Some((n, t)) => (n, t.trim()),
                    None => (arg, ""),
                };
                if pname == "list" || pname.is_empty() {
                    if self.ai_presets.is_empty() {
                        self.ai_output.push("  no presets defined".into());
                    } else {
                        self.ai_output
                            .push(format!("  presets ({}):", self.ai_presets.len()));
                        for (name, text) in self.ai_presets.iter() {
                            let preview: String = text.chars().take(60).collect();
                            self.ai_output
                                .push(format!("    /preset {name} — {preview}"));
                        }
                    }
                } else if pname == "del" && !ptext.is_empty() {
                    if self.ai_presets.remove(ptext).is_some() {
                        self.ai_save_presets();
                        self.ai_output.push(format!("  preset '{ptext}' deleted"));
                    } else {
                        self.ai_output.push(format!("  preset '{ptext}' not found"));
                    }
                } else if !ptext.is_empty() {
                    self.ai_presets.insert(pname.to_string(), ptext.to_string());
                    self.ai_save_presets();
                    self.ai_output.push(format!("  preset '{pname}' saved"));
                } else if let Some(text) = self.ai_presets.get(pname) {
                    self.ai_system_prompt = text.clone();
                    self.ai_output
                        .push(format!("  preset '{pname}' applied as system prompt"));
                } else {
                    self.ai_output.push(format!(
                        "  preset '{pname}' not found — define: /preset {pname} <text>"
                    ));
                }
            }
            "save" => {
                self.ai_save_chat();
                self.ai_output
                    .push(format!("  chat saved to {}", chat_path().display()));
            }
            "load" => {
                self.ai_restore_chat();
                self.ai_output
                    .push(format!("  chat restored from {}", chat_path().display()));
            }
            "status" => {
                let backend = match self.ai_config.backend {
                    aios_llm::BackendKind::Cloud(ref p) => {
                        format!("cloud/{}", aios_llm::provider_name(p))
                    }
                    aios_llm::BackendKind::MicroLocal => "local/micro".into(),
                    aios_llm::BackendKind::FullLocal => "local/full".into(),
                };
                self.ai_output.push(format!(
                    "  {backend} | {} | temp {} | tokens {}",
                    self.ai_config.model, self.ai_config.temperature, self.ai_config.max_tokens
                ));
            }
            "model" => {
                if arg.is_empty() {
                    self.ai_output
                        .push(format!("  model: {}", self.ai_config.model));
                } else {
                    self.ai_config.model = arg.to_string();
                    self.ai_output.push(format!("  model set to '{arg}'"));
                }
            }
            "backend" => {
                match arg {
                    "groq" => {
                        self.ai_config.backend =
                            aios_llm::BackendKind::Cloud(aios_llm::CloudProvider::Groq);
                    }
                    "openrouter" => {
                        self.ai_config.backend =
                            aios_llm::BackendKind::Cloud(aios_llm::CloudProvider::OpenRouter);
                    }
                    "google" => {
                        self.ai_config.backend =
                            aios_llm::BackendKind::Cloud(aios_llm::CloudProvider::GoogleAiStudio);
                    }
                    "micro" => self.ai_config.backend = aios_llm::BackendKind::MicroLocal,
                    "full" => self.ai_config.backend = aios_llm::BackendKind::FullLocal,
                    _ => {
                        self.ai_output
                            .push("  backend: groq | openrouter | google | micro | full".into());
                        return;
                    }
                }
                if let aios_llm::BackendKind::Cloud(ref p) = self.ai_config.backend {
                    self.ai_config.model = p.default_model().to_string();
                }
                self.ai_output.push(format!("  backend set to '{arg}'"));
            }
            "key" => {
                if arg.is_empty() {
                    self.ai_config.api_key = None;
                    self.ai_output.push("  api key cleared".into());
                } else {
                    self.ai_config.api_key = Some(arg.to_string());
                    self.ai_output.push("  api key set".into());
                }
            }
            "temp" => match arg.parse::<f32>() {
                Ok(t) if (0.0..=2.0).contains(&t) => {
                    self.ai_config.temperature = t;
                    self.ai_output.push(format!("  temperature set to {t}"));
                }
                _ => self.ai_output.push("  temp: usage /temp <0.0-2.0>".into()),
            },
            "tokens" => match arg.parse::<u32>() {
                Ok(n) if (1..=8192).contains(&n) => {
                    self.ai_config.max_tokens = n;
                    self.ai_output.push(format!("  max tokens set to {n}"));
                }
                _ => self
                    .ai_output
                    .push("  tokens: usage /tokens <1-8192>".into()),
            },
            _ => {
                self.ai_output
                    .push(format!("  unknown command '/{name}' — type /help"));
            }
        }
    }

    /// Writes the chat log as JSON Lines (same schema as the TUI AI Console).
    pub fn ai_save_chat(&mut self) {
        let path = chat_path();
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let mut buf = String::new();
        for msg in &self.ai_log {
            if let Ok(line) = serde_json::to_string(msg) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        let _ = std::fs::write(path, buf);
    }

    /// Writes the prompt templates as a JSON object.
    pub fn ai_save_presets(&mut self) {
        let path = presets_path();
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.ai_presets) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Restores a previously saved chat from disk, replacing the transcript.
    pub fn ai_restore_chat(&mut self) {
        let Ok(content) = std::fs::read_to_string(chat_path()) else {
            return;
        };
        let mut messages: Vec<AiMessage> = Vec::new();
        for line in content.lines() {
            if let Ok(msg) = serde_json::from_str::<AiMessage>(line) {
                messages.push(msg);
            }
        }
        if messages.is_empty() {
            return;
        }
        self.ai_log = messages;
        self.ai_output.clear();
        for msg in &self.ai_log {
            if msg.role == "user" {
                self.ai_output.push(format!("> {}", msg.text));
            } else {
                self.ai_output.push(msg.text.clone());
            }
        }
        self.ai_status = "chat restored from disk".into();
    }

    /// Overlays persisted presets over the built-in seeds at boot.
    pub fn ai_load_presets(&mut self) {
        let Ok(content) = std::fs::read_to_string(presets_path()) else {
            return;
        };
        let Ok(saved) = serde_json::from_str::<BTreeMap<String, String>>(&content) else {
            return;
        };
        for (name, text) in saved {
            if !name.trim().is_empty() && !text.trim().is_empty() {
                self.ai_presets.insert(name, text);
            }
        }
    }

    /// Loads persisted chat + presets (called once at startup).
    pub fn ai_load_persisted(&mut self) {
        self.ai_load_presets();
        self.ai_restore_chat();
    }

    pub fn net_save(&mut self) {
        let json = self.net_config.to_json();
        self.net_status = Some(format!("Saved: {json}"));
        self.add_log("Network config saved".into());
    }

    pub fn net_reset(&mut self) {
        self.net_config = NetworkConfig::default();
        self.net_status = Some("Reset to defaults".into());
        self.add_log("Network config reset".into());
    }

    fn move_selection_up(&mut self) {
        match self.selected_tab {
            1 => {
                if self.selected_block_idx.is_some_and(|i| i > 0) {
                    self.selected_block_idx = self.selected_block_idx.map(|i| i - 1);
                }
            }
            3 if self.selected_marketplace_idx.is_some_and(|i| i > 0) => {
                self.selected_marketplace_idx = self.selected_marketplace_idx.map(|i| i - 1);
            }
            _ => {}
        }
    }

    fn move_selection_down(&mut self) {
        match self.selected_tab {
            1 => {
                let max = self.blocks.len();
                if max > 0 && self.selected_block_idx.is_none_or(|i| i < max - 1) {
                    self.selected_block_idx = Some(self.selected_block_idx.map_or(0, |i| i + 1));
                }
            }
            3 => {
                let max = self.marketplace_entries.len();
                if max > 0 && self.selected_marketplace_idx.is_none_or(|i| i < max - 1) {
                    self.selected_marketplace_idx =
                        Some(self.selected_marketplace_idx.map_or(0, |i| i + 1));
                }
            }
            _ => {}
        }
    }
}

impl Drop for AiosApp {
    fn drop(&mut self) {
        if !self.ai_log.is_empty() {
            self.ai_save_chat();
        }
    }
}

impl eframe::App for AiosApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let theme = AiosTheme::default();
        theme.apply(ctx);

        self.poll_browser_open();
        self.poll_ai();
        self.poll_fm_acks();
        self.hw_poll_hotplug();

        if self.selected_tab == 8 {
            self.hw_refresh();
        }

        if self.ai_busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(fm) = self.fm.as_ref() {
            if fm
                .snapshot()
                .jobs
                .iter()
                .any(|j| j.status == aios_fm::engine::JobStatus::Running)
            {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        self.uptime_secs += 1;
        self.ipc_traffic = self.ipc_traffic.wrapping_add(1);

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("AIOS v1.0.0")
                        .color(theme.accent)
                        .size(16.0)
                        .strong(),
                );
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("{}", self.ai_tier))
                        .color(theme.success)
                        .size(13.0),
                );
                ui.separator();
                let wd_text = match self.watchdog_state {
                    0 => "WD: OK",
                    1 => "WD: SUSPENDED",
                    2 => "WD: RECOVERING",
                    _ => "WD: SAFE MODE",
                };
                let wd_color = if self.watchdog_state == 0 {
                    theme.success
                } else {
                    theme.danger
                };
                ui.label(egui::RichText::new(wd_text).color(wd_color).size(12.0));
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("RAM: {}/{} MB", self.ram_used, self.ram_total))
                        .color(theme.text)
                        .size(12.0),
                );
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("Blocks: {}", self.blocks.len()))
                        .color(theme.success)
                        .size(12.0),
                );
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("Proc: {}", self.processes.len()))
                        .color(theme.success)
                        .size(12.0),
                );
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("Up: {}s", self.uptime_secs))
                        .color(theme.text_dim)
                        .size(12.0),
                );
            });
        });

        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "HW Tier: {} | IPC: {} pkts | F6=Deps F7=Browser F8=Files F9=Hardware",
                        self.ai_tier, self.ipc_traffic
                    ))
                    .color(theme.text_dim)
                    .size(11.0),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("AIOS Dashboard")
                            .color(theme.muted)
                            .size(11.0),
                    );
                });
            });
        });

        egui::SidePanel::left("tabs")
            .exact_width(160.0)
            .frame(
                egui::Frame::new()
                    .fill(theme.surface)
                    .inner_margin(egui::Margin::symmetric(0, 8))
                    .stroke(egui::Stroke::new(1.0_f32, theme.border)),
            )
            .show(ctx, |ui| {
                ui.add_space(4.0);
                let tabs = [
                    ("\u{2302} System Dashboard", 0),
                    ("\u{2b23} WASM Blocks", 1),
                    ("\u{2728} AI Studio", 2),
                    ("\u{1f4e6} App Store", 3),
                    ("\u{1f4e1} Network Settings", 4),
                    ("\u{2913} Deps", 5),
                    ("\u{1f310} Native Browser", 6),
                    ("\u{1f4c1} Files", 7),
                    ("\u{1f527} Hardware", 8),
                ];

                for (label, idx) in tabs {
                    let is_active = self.selected_tab == idx;
                    let text_color = if is_active { theme.accent } else { theme.text };

                    let resp = ui.add(
                        egui::Button::new(egui::RichText::new(label).color(text_color).size(13.0))
                            .fill(if is_active {
                                theme.accent.linear_multiply(0.12)
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(6.0)
                            .min_size(egui::vec2(140.0, 32.0)),
                    );
                    if resp.clicked() {
                        self.selected_tab = idx;
                    }
                    ui.add_space(2.0);
                }

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(8.0);

                ui.label(
                    egui::RichText::new("Quick Actions")
                        .color(theme.muted)
                        .size(10.0),
                );
                ui.add_space(4.0);

                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("\u{21bb} Refresh All")
                                .color(theme.info)
                                .size(11.0),
                        )
                        .fill(theme.button_bg)
                        .corner_radius(4.0)
                        .min_size(egui::vec2(130.0, 24.0)),
                    )
                    .clicked()
                {
                    self.add_log("Full refresh".into());
                }

                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("\u{23f8} Suspend All")
                                .color(theme.warning)
                                .size(11.0),
                        )
                        .fill(theme.button_bg)
                        .corner_radius(4.0)
                        .min_size(egui::vec2(130.0, 24.0)),
                    )
                    .clicked()
                {
                    self.add_log("Suspend all processes".into());
                }
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme.surface)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| match self.selected_tab {
                0 => tabs::overview::show(ui, self, &theme),
                1 => tabs::blocks::show(ui, self, &theme),
                2 => tabs::ai_studio::show(ui, self, &theme),
                3 => tabs::marketplace::show(ui, self, &theme),
                4 => tabs::network::show(ui, self, &theme),
                5 => tabs::deps::show(ui, self, &theme),
                6 => tabs::web::show(ui, self, &theme),
                7 => tabs::files::show(ui, self, &theme),
                8 => tabs::hardware::show(ui, self, &theme),
                _ => tabs::overview::show(ui, self, &theme),
            });

        let fm_active = self.selected_tab == 7 && self.fm_input.is_none();
        let fm_side = self.fm.as_ref().map(|fm| fm.active_side());
        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Key {
                    key: k,
                    pressed: true,
                    ..
                } = event
                {
                    if fm_active {
                        if let Some(side) = fm_side {
                            if let Some(action) = Self::fm_key_action(*k, side) {
                                self.fm_act(action);
                                continue;
                            }
                        }
                    }
                    match k {
                        egui::Key::F1 => self.selected_tab = 0,
                        egui::Key::F2 => self.selected_tab = 1,
                        egui::Key::F3 => self.selected_tab = 2,
                        egui::Key::F4 => self.selected_tab = 3,
                        egui::Key::F5 => self.selected_tab = 4,
                        egui::Key::F6 => self.selected_tab = 5,
                        egui::Key::F7 => self.selected_tab = 6,
                        egui::Key::F8 => self.selected_tab = 7,
                        egui::Key::F9 => self.selected_tab = 8,
                        egui::Key::J => self.move_selection_down(),
                        egui::Key::K => self.move_selection_up(),
                        _ => {}
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_creation() {
        let app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            vec!["a".into(), "b".into()],
            vec!["a".into(), "b".into()],
            vec![("a".into(), "b".into())],
            2,
            65536,
        );
        assert_eq!(app.dep_blocks.len(), 2);
        assert_eq!(app.ram_total, 65536);
        assert_eq!(app.selected_tab, 0);
    }

    #[test]
    fn test_add_log() {
        let mut app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            4096,
        );
        app.add_log("test".into());
        assert_eq!(app.log_messages.len(), 4);
    }

    #[test]
    fn test_log_limit() {
        let mut app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            4096,
        );
        for i in 0..250 {
            app.add_log(format!("msg {i}"));
        }
        assert_eq!(app.log_messages.len(), 200);
    }

    #[test]
    fn test_tab_navigation() {
        let mut app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            4096,
        );
        app.selected_tab = 1;
        assert_eq!(app.selected_tab, 1);
    }

    #[test]
    fn test_block_operations() {
        let mut app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            4096,
        );
        app.load_block("test".into(), "0.1.0".into());
        assert!(app.log_messages.last().unwrap().contains("Loading"));
        app.unload_block(0);
        assert!(app.log_messages.last().unwrap().contains("Unloading"));
    }

    #[test]
    fn test_marketplace_search() {
        let mut app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            4096,
        );
        app.marketplace_entries.push(MarketplaceEntry {
            name: "test-block".into(),
            version: "1.0.0".into(),
            author: "test".into(),
            description: "A test block".into(),
            status: "Available".into(),
            tags: vec!["test".into()],
            downloads: 42,
        });
        app.marketplace_search = "test".into();
        app.search_marketplace();
        assert!(app.marketplace_status.as_deref().unwrap().contains("1"));
    }

    #[test]
    fn test_marketplace_install() {
        let mut app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            4096,
        );
        app.install_block("my-block".into());
        assert!(app.log_messages.last().unwrap().contains("Installing"));
    }

    #[test]
    fn test_browser_closed_actions_error() {
        let mut app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            4096,
        );
        assert!(!app.browser_active());
        assert!(app.browser_back().is_err());
        assert!(app.browser_forward().is_err());
        app.browser_status = None;
        app.close_browser();
        assert!(app.browser_status.is_none());
        assert!(!app.browser_active());
    }

    #[test]
    fn test_fm_key_action_mapping() {
        use aios_fm::ui_tui::TuiAction;
        assert!(matches!(
            AiosApp::fm_key_action(egui::Key::F5, PanelSide::Left),
            Some(TuiAction::CopySelected)
        ));
        assert!(matches!(
            AiosApp::fm_key_action(egui::Key::F7, PanelSide::Right),
            Some(TuiAction::Mkdir {
                side: PanelSide::Right
            })
        ));
        assert!(matches!(
            AiosApp::fm_key_action(egui::Key::Tab, PanelSide::Left),
            Some(TuiAction::SwitchPanel)
        ));
        assert!(AiosApp::fm_key_action(egui::Key::F1, PanelSide::Left).is_none());
    }

    #[test]
    fn test_fm_init_and_mkdir() {
        let mut app = AiosApp::new(
            AiTier::Tier1,
            HardwareProfile::mock_modern(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            4096,
        );
        app.fm_init();
        assert!(app.fm.is_some());
        assert!(app.fm_rt.is_some());

        app.fm_act(aios_fm::ui_tui::TuiAction::Mkdir {
            side: PanelSide::Left,
        });
        app.fm_input_buf = "gui_test_dir".into();
        app.fm_confirm_input();

        let mut seen_created = false;
        for _ in 0..200 {
            app.poll_fm_acks();
            if app
                .log_messages
                .iter()
                .any(|l| l.contains("created") && l.contains("gui_test_dir"))
            {
                seen_created = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            seen_created,
            "expected mkdir ack in log: {:?}",
            app.log_messages
        );
    }
}
