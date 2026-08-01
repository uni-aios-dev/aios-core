use crate::tabs;
use crate::theme::AiosTheme;

use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;

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

    pub uptime_secs: u64,
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
            uptime_secs: 0,
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

    pub fn open_browser(&mut self) -> Result<(), String> {
        if self.browser.is_some() {
            return Ok(());
        }
        let target = aios_webview::resolve_target(self.browser_addr.trim());
        let browser = aios_webview::WebBrowser::open(&target)?;
        self.browser = Some(browser);
        self.browser_status = Some(format!("Opened: {target}"));
        self.add_log(format!("Browser opened: {target}"));
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
                let browser = aios_webview::WebBrowser::open(&target)?;
                self.browser = Some(browser);
                self.browser_status = Some(format!("Opened: {target}"));
            }
        }
        self.add_log(format!("Browser -> {target}"));
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
        if self.browser.is_some() {
            self.browser = None;
            self.browser_status = Some("Browser closed".into());
            self.add_log("Browser closed".into());
        }
    }

    fn move_selection_up(&mut self) {
        match self.selected_tab {
            1 => {
                if self.selected_process_idx.is_some_and(|i| i > 0) {
                    self.selected_process_idx = self.selected_process_idx.map(|i| i - 1);
                }
            }
            2 => {
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
                let max = self.processes.len();
                if max > 0 && self.selected_process_idx.is_none_or(|i| i < max - 1) {
                    self.selected_process_idx =
                        Some(self.selected_process_idx.map_or(0, |i| i + 1));
                }
            }
            2 => {
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

impl eframe::App for AiosApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let theme = AiosTheme::default();
        theme.apply(ctx);

        self.uptime_secs += 1;

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
                    egui::RichText::new(
                        "F1=Overview F2=Processes F3=Blocks F4=Marketplace F5=Metrics F6=Deps F7=Browser",
                    )
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
                    ("\u{2302} Overview", 0),
                    ("\u{25b6} Processes", 1),
                    ("\u{2b23} Blocks", 2),
                    ("\u{1f4e6} Marketplace", 3),
                    ("\u{2630} Metrics", 4),
                    ("\u{2913} Deps", 5),
                    ("\u{1f310} Browser", 6),
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
                        .fill(theme.surface_alt)
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
                        .fill(theme.surface_alt)
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
                1 => tabs::processes::show(ui, self, &theme),
                2 => tabs::blocks::show(ui, self, &theme),
                3 => tabs::marketplace::show(ui, self, &theme),
                4 => tabs::metrics::show(ui, self, &theme),
                5 => tabs::deps::show(ui, self, &theme),
                6 => tabs::web::show(ui, self, &theme),
                _ => tabs::overview::show(ui, self, &theme),
            });

        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Key {
                    key: k,
                    pressed: true,
                    ..
                } = event
                {
                    match k {
                        egui::Key::F1 => self.selected_tab = 0,
                        egui::Key::F2 => self.selected_tab = 1,
                        egui::Key::F3 => self.selected_tab = 2,
                        egui::Key::F4 => self.selected_tab = 3,
                        egui::Key::F5 => self.selected_tab = 4,
                        egui::Key::F6 => self.selected_tab = 5,
                        egui::Key::F7 => self.selected_tab = 6,
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
        assert!(app.marketplace_status.unwrap().contains("1"));
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
}
