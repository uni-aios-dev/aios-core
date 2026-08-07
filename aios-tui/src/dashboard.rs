use std::sync::{Arc, Mutex};

use aios_block_mgr::registry::BlockRegistry;
use aios_fm::commands::Ack;
use aios_fm::engine::FileManager;
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_process_mgr::scheduler::Scheduler;
use aios_vfs::ai_preview::AiPreview;
use aios_watchdog::watchdog::WatchdogState;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, List, ListItem, Paragraph, Row, Table, Tabs},
    Frame,
};
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Clone, Debug)]
pub struct PageContent {
    pub url: String,
    pub title: String,
    pub text: String,
    pub links: Vec<(String, String)>,
}

/// Number of link rows visible in the Web tab links window.
pub const LINKS_VIEW_ROWS: usize = 6;

/// Upper bound for the in-memory web page cache (URL-keyed, oldest evicted).
pub const WEB_CACHE_CAP: usize = 20;

/// Fixed width of the Web tab navigation sidebar (history list).
pub const SIDEBAR_WIDTH: usize = 26;

/// A single entry in the Web tab navigation sidebar.
#[derive(Clone, Debug)]
pub struct NavEntry {
    pub label: String,
    pub url: String,
    pub is_current: bool,
}

/// Columns available for page text wrapping, given the terminal width and the
/// fixed navigation sidebar (sidebar + page borders + 2-col line prefix).
pub fn web_page_width(term_width: usize) -> usize {
    term_width.saturating_sub(SIDEBAR_WIDTH + 4).max(4)
}

/// Short, human-readable label for a URL, truncated to `max` columns.
pub fn compact_url_label(url: &str, max: usize) -> String {
    let mut s = url.trim();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            s = rest;
            break;
        }
    }
    if let Some(rest) = s.strip_prefix("www.") {
        s = rest;
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return s.chars().take(max).collect();
    }
    let mut truncated: String = s.chars().take(max - 1).collect();
    truncated.push('…');
    truncated
}

/// Build the navigation sidebar entries: the current page first (marked), then
/// the visited history newest-first, deduplicated against already listed URLs.
pub fn web_nav_entries(ws: &WebState) -> Vec<NavEntry> {
    let mut out = Vec::new();
    if !ws.current_url.is_empty() {
        out.push(NavEntry {
            label: compact_url_label(&ws.current_url, SIDEBAR_WIDTH - 4),
            url: ws.current_url.clone(),
            is_current: true,
        });
    }
    for url in ws.history.iter().rev() {
        if url.is_empty() || out.iter().any(|e| e.url == *url) {
            continue;
        }
        out.push(NavEntry {
            label: compact_url_label(url, SIDEBAR_WIDTH - 4),
            url: url.clone(),
            is_current: false,
        });
    }
    out
}

/// Outbox for background web fetches: `(fetch generation, result)`.
pub type WebFetchOutbox = Arc<Mutex<Option<(u64, Result<(PageContent, Option<String>), String>)>>>;

#[derive(Clone, Debug)]
pub struct ShellState {
    pub input_buffer: String,
    pub output: Vec<String>,
    pub command_history: Vec<String>,
    pub history_pos: usize,
    pub show_bar: bool,
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellState {
    pub fn new() -> Self {
        Self {
            input_buffer: String::new(),
            output: vec!["AIOS Shell — type 'help' for commands".into()],
            command_history: Vec::new(),
            history_pos: 0,
            show_bar: false,
        }
    }

    pub fn add_output(&mut self, line: String) {
        self.output.push(line);
        if self.output.len() > 500 {
            self.output.remove(0);
        }
    }

    pub fn push_history(&mut self, cmd: String) {
        self.command_history.push(cmd);
        self.history_pos = self.command_history.len();
    }
}

#[derive(Clone, Debug)]
pub struct WebState {
    pub url_input: String,
    pub current_url: String,
    /// Last plain-text search query; shown in the omnibox as `search: <query>`.
    pub search_query: String,
    pub page: Option<PageContent>,
    pub loading: bool,
    pub error: Option<String>,
    pub input_focused: bool,
    pub scroll: usize,
    /// Scroll offset of the links list window (keeps the selection visible).
    pub links_scroll: usize,
    /// Previously visited URLs, newest last; `b` pops back to the previous one.
    pub history: Vec<String>,
    /// Bounded in-memory cache of fetched pages keyed by URL.
    pub cache: Vec<(String, PageContent)>,
    /// Monotonic id of the latest web fetch; stale background results are dropped.
    pub web_fetch_gen: u64,
    /// Terminal width used to pre-wrap `page.text` into visual lines.
    pub wrap_width: usize,
    /// Whether the navigation sidebar has keyboard focus (`\` toggles it).
    pub sidebar_focused: bool,
    /// Selected entry index in the navigation sidebar list.
    pub history_sel: usize,
}

impl WebState {
    /// Insert a fetched page into the bounded cache, evicting the oldest entry
    /// when the cap is reached.
    pub fn cache_page(&mut self, page: PageContent) {
        let key = page.url.clone();
        self.cache.retain(|(u, _)| u != &key);
        self.cache.push((key, page));
        while self.cache.len() > WEB_CACHE_CAP {
            self.cache.remove(0);
        }
    }

    /// Look up a previously fetched page by URL, newest match first.
    pub fn cached_page(&self, url: &str) -> Option<PageContent> {
        self.cache
            .iter()
            .rev()
            .find(|(u, _)| u == url)
            .map(|(_, p)| p.clone())
    }
}

/// Word-wrap `text` so every line is at most `width` columns wide. Over-long
/// words are hard-split; existing blank lines and leading indentation are
/// preserved.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let indent = &raw[..raw.len() - raw.trim_start().len()];
        let mut line = String::new();
        let mut start_of_line = true;
        let mut has_content = false;
        for word in raw.split_whitespace() {
            if has_content && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
                has_content = false;
            }
            if has_content {
                line.push(' ');
            } else if start_of_line {
                line.push_str(indent);
                start_of_line = false;
            }
            let mut rest = word.to_string();
            while !rest.is_empty() && line.chars().count() + rest.chars().count() > width {
                let avail = width.saturating_sub(line.chars().count()).max(1);
                let cut: String = rest.chars().take(avail).collect();
                line.push_str(&cut);
                out.push(std::mem::take(&mut line));
                rest = rest.chars().skip(avail).collect();
            }
            line.push_str(&rest);
            has_content = !line.is_empty();
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

pub struct ProcessSnapshot {
    pub pid: u64,
    pub name: String,
    pub priority: String,
    pub state: String,
    pub ram_mb: u64,
    pub cpu_ms: u64,
    pub crashes: u32,
}

pub struct BlockSnapshot {
    pub id: u32,
    pub name: String,
    pub version: String,
    pub state: String,
    pub size: usize,
    pub deps: Vec<String>,
    pub dependents: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DependencySnapshot {
    pub blocks: Vec<String>,
    pub load_order: Vec<String>,
    pub edges: Vec<(String, String)>,
}

pub struct DashboardState {
    pub ai_tier: AiTier,
    pub hardware: HardwareProfile,
    pub blocks_count: usize,
    pub process_count: usize,
    pub ram_used: u64,
    pub ram_total: u64,
    pub log_messages: Vec<String>,
    pub selected_tab: usize,
    pub watchdog_state: WatchdogState,
    pub processes: Vec<ProcessSnapshot>,
    pub blocks: Vec<BlockSnapshot>,
    pub selected_row: usize,
    pub ram_history: Vec<u64>,
    pub process_kill_result: Option<String>,
    pub block_operation_result: Option<String>,
    pub block_input_mode: BlockInputMode,
    pub block_input_buffer: String,
    pub dep_snapshot: DependencySnapshot,
    pub web_state: WebState,
    /// Outbox for background web fetches: `(fetch generation, result)`.
    pub page_cache: WebFetchOutbox,
    pub shell_state: ShellState,
    pub show_help: bool,
    /// File manager instance (only present when the tokio runtime is active).
    pub fm: Option<FileManager>,
    /// Outbox for FM job acknowledgements.
    pub fm_ack: Option<UnboundedReceiver<Ack>>,
    /// AI preview of the currently viewed file (Files tab).
    pub fm_preview: Option<AiPreview>,
    /// Modal input mode for the Files tab (F7 mkdir / F2 rename).
    pub fm_input_mode: FmInputMode,
    /// Buffer for the active Files-tab modal input.
    pub fm_input_buffer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockInputMode {
    None,
    LoadName,
    LoadVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmInputMode {
    None,
    Mkdir,
    Rename,
}

impl DashboardState {
    pub fn new(
        ai_tier: AiTier,
        hardware: HardwareProfile,
        registry: &BlockRegistry,
        scheduler: &Scheduler,
    ) -> Self {
        let (ram_used, ram_total) = scheduler.ram_usage();
        let processes = Self::snapshot_processes(scheduler);
        let blocks = Self::snapshot_blocks(registry);
        Self {
            ai_tier,
            hardware,
            blocks_count: registry.count(),
            process_count: scheduler.process_count(),
            ram_used,
            ram_total,
            log_messages: vec![
                "System initialized".into(),
                format!("AI Tier: {}", ai_tier),
                format!("{} blocks loaded", registry.count()),
            ],
            selected_tab: 0,
            watchdog_state: WatchdogState::Monitoring,
            processes,
            blocks,
            selected_row: 0,
            ram_history: vec![ram_used],
            process_kill_result: None,
            block_operation_result: None,
            block_input_mode: BlockInputMode::None,
            block_input_buffer: String::new(),
            dep_snapshot: Self::snapshot_dependencies(registry),
            web_state: WebState {
                url_input: String::new(),
                current_url: String::new(),
                search_query: String::new(),
                page: None,
                loading: false,
                error: None,
                input_focused: false,
                scroll: 0,
                links_scroll: 0,
                history: Vec::new(),
                cache: Vec::new(),
                web_fetch_gen: 0,
                wrap_width: 78,
                sidebar_focused: false,
                history_sel: 0,
            },
            page_cache: Arc::new(Mutex::new(None)),
            shell_state: ShellState::default(),
            show_help: false,
            fm: None,
            fm_ack: None,
            fm_preview: None,
            fm_input_mode: FmInputMode::None,
            fm_input_buffer: String::new(),
        }
    }

    pub fn add_log(&mut self, msg: String) {
        self.log_messages.push(msg);
        if self.log_messages.len() > 100 {
            self.log_messages.remove(0);
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
            let msg = match &ack {
                Ack::DirChanged { side } => format!("FM: {} refreshed", side.name()),
                Ack::Copied(s) => format!("FM: copied {} files ({} B)", s.files, s.bytes),
                Ack::Moved(s) => format!("FM: moved {} files", s.files),
                Ack::Deleted(s) => format!("FM: deleted {} files ({} B)", s.files, s.bytes),
                Ack::CreatedDir { path } => format!("FM: created {}", path.to_uri()),
                Ack::Renamed { from, to } => {
                    format!("FM: renamed {} -> {}", from.to_uri(), to.to_uri())
                }
                Ack::View { preview, .. } => {
                    self.fm_preview = Some(preview.clone());
                    format!("FM: {}", preview.headline())
                }
                Ack::Error(e) => format!("FM: error: {e}"),
            };
            self.add_log(msg);
        }
    }

    /// Confirm the active Files-tab modal input (F7 mkdir / F2 rename).
    pub fn fm_confirm_input(&mut self) {
        let name = self.fm_input_buffer.trim().to_string();
        let mode = self.fm_input_mode;
        self.fm_input_mode = FmInputMode::None;
        self.fm_input_buffer.clear();
        if name.is_empty() {
            return;
        }
        if let Some(fm) = self.fm.as_ref() {
            let side = fm.active_side();
            match mode {
                FmInputMode::Mkdir => {
                    fm.send(aios_fm::commands::Command::Mkdir {
                        side,
                        parent: fm.panel_path(side),
                        name,
                    });
                    self.add_log("FM: creating directory...".into());
                }
                FmInputMode::Rename => {
                    if let Some(from) = fm.selected(side) {
                        let to = from.parent().join(&name);
                        fm.send(aios_fm::commands::Command::Rename { side, from, to });
                        self.add_log("FM: renaming...".into());
                    }
                }
                FmInputMode::None => {}
            }
        }
    }

    pub fn update_from_scheduler(&mut self, scheduler: &Scheduler, registry: &BlockRegistry) {
        let (used, total) = scheduler.ram_usage();
        self.ram_used = used;
        self.ram_total = total;
        self.process_count = scheduler.process_count();
        self.processes = Self::snapshot_processes(scheduler);
        self.blocks = Self::snapshot_blocks(registry);
        self.dep_snapshot = Self::snapshot_dependencies(registry);
        self.ram_history.push(used);
        if self.ram_history.len() > 60 {
            self.ram_history.remove(0);
        }
    }

    pub fn update_watchdog(&mut self, state: WatchdogState) {
        self.watchdog_state = state;
    }

    pub fn move_selection_up(&mut self) {
        if self.selected_row > 0 {
            self.selected_row -= 1;
        }
        self.clamp_web_links_scroll();
    }

    pub fn move_selection_down(&mut self) {
        let max = match self.selected_tab {
            1 => self.processes.len(),
            2 => self.blocks.len(),
            4 => self.dep_snapshot.blocks.len(),
            5 => {
                if let Some(ref page) = self.web_state.page {
                    page.links.len()
                } else {
                    0
                }
            }
            6 => self.shell_state.output.len(),
            _ => 0,
        };
        if max > 0 && self.selected_row < max - 1 {
            self.selected_row += 1;
        }
        self.clamp_web_links_scroll();
    }

    /// Keep the Web tab links window scrolled so the selected row stays visible.
    fn clamp_web_links_scroll(&mut self) {
        if self.selected_tab != 5 {
            return;
        }
        let max_start = self
            .web_state
            .page
            .as_ref()
            .map(|p| p.links.len().saturating_sub(LINKS_VIEW_ROWS))
            .unwrap_or(0);
        self.web_state.links_scroll = self
            .selected_row
            .saturating_sub(LINKS_VIEW_ROWS - 1)
            .min(max_start);
    }

    /// Pick up the result of a background web fetch, ignoring stale generations.
    pub fn check_page_cache(&mut self) {
        let content = self.page_cache.lock().ok().and_then(|mut c| c.take());
        if let Some((gen, result)) = content {
            if gen != self.web_state.web_fetch_gen {
                return;
            }
            match result {
                Ok((page, search_query)) => {
                    let url = page.url.clone();
                    self.web_state.cache_page(page.clone());
                    self.web_state.page = Some(page);
                    self.web_state.current_url = url.clone();
                    self.web_state.url_input.clear();
                    self.web_state.search_query = search_query.unwrap_or_default();
                    self.web_state.loading = false;
                    self.web_state.error = None;
                    self.web_state.scroll = 0;
                    self.web_state.links_scroll = 0;
                    self.selected_row = 0;
                    self.add_log(format!("Web: loaded {url}"));
                }
                Err(e) => {
                    self.web_state.loading = false;
                    self.web_state.error = Some(e);
                    self.add_log("Web: fetch failed".into());
                }
            }
        }
    }

    pub fn selected_process_pid(&self) -> Option<u64> {
        if self.selected_tab == 1 {
            self.processes.get(self.selected_row).map(|p| p.pid)
        } else {
            None
        }
    }

    pub fn selected_block_name_version(&self) -> Option<(String, String)> {
        if self.selected_tab == 2 {
            self.blocks
                .get(self.selected_row)
                .map(|b| (b.name.clone(), b.version.clone()))
        } else {
            None
        }
    }

    pub fn start_load_block(&mut self) {
        self.block_input_mode = BlockInputMode::LoadName;
        self.block_input_buffer.clear();
        self.block_operation_result = None;
    }

    pub fn cancel_block_input(&mut self) {
        self.block_input_mode = BlockInputMode::None;
        self.block_input_buffer.clear();
    }

    pub fn push_char_to_block_input(&mut self, c: char) {
        self.block_input_buffer.push(c);
    }

    pub fn pop_char_from_block_input(&mut self) {
        self.block_input_buffer.pop();
    }

    pub fn confirm_block_load(&mut self) -> Option<(String, String)> {
        match self.block_input_mode {
            BlockInputMode::LoadName => {
                let name = self.block_input_buffer.trim().to_string();
                if name.is_empty() {
                    return None;
                }
                self.block_input_mode = BlockInputMode::LoadVersion;
                self.block_input_buffer.clear();
                Some(("__name__".into(), name))
            }
            BlockInputMode::LoadVersion => {
                let version = self.block_input_buffer.trim().to_string();
                self.block_input_mode = BlockInputMode::None;
                self.block_input_buffer.clear();
                Some(("__version__".into(), version))
            }
            BlockInputMode::None => None,
        }
    }

    fn snapshot_processes(scheduler: &Scheduler) -> Vec<ProcessSnapshot> {
        scheduler
            .all_processes()
            .iter()
            .map(|p| ProcessSnapshot {
                pid: p.pid.0,
                name: p.name.clone(),
                priority: format!("{:?}", p.priority),
                state: format!("{:?}", p.state),
                ram_mb: p.ram_quota_mb,
                cpu_ms: p.cpu_time_ms,
                crashes: p.crash_count,
            })
            .collect()
    }

    fn snapshot_blocks(registry: &BlockRegistry) -> Vec<BlockSnapshot> {
        let graph = registry.dependency_graph();
        registry
            .topology_with_state()
            .iter()
            .map(|(m, s)| BlockSnapshot {
                id: m.id.0,
                name: m.name.clone(),
                version: m.version.clone(),
                state: format!("{s:?}"),
                size: registry.get(m.id).map(|e| e.binary.len()).unwrap_or(0),
                deps: graph.dependencies_of(&m.name),
                dependents: graph.dependents_of(&m.name),
            })
            .collect()
    }

    fn snapshot_dependencies(registry: &BlockRegistry) -> DependencySnapshot {
        let graph = registry.dependency_graph();
        let blocks: Vec<String> = graph.blocks().into_iter().map(String::from).collect();
        let load_order = graph.load_order().unwrap_or_default();
        let mut edges = Vec::new();
        for block in &blocks {
            for dep in graph.dependencies_of(block) {
                edges.push((block.clone(), dep));
            }
        }
        DependencySnapshot {
            blocks,
            load_order,
            edges,
        }
    }
}

pub fn draw_dashboard(f: &mut Frame<'_>, state: &DashboardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(f.area());

    draw_header(f, chunks[0], state);
    draw_tabs(f, chunks[1], state);
    draw_main(f, chunks[2], state);
    draw_footer(f, chunks[3]);

    if state.show_help {
        draw_help(f, f.area());
    }
}

fn draw_header(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let wd_color = match state.watchdog_state {
        WatchdogState::Monitoring => Color::Green,
        WatchdogState::Warned => Color::Yellow,
        WatchdogState::Suspended => Color::Red,
        WatchdogState::Recovering => Color::Yellow,
        WatchdogState::SafeMode => Color::Magenta,
    };
    let wd_label = match state.watchdog_state {
        WatchdogState::Monitoring => "OK",
        WatchdogState::Warned => "WARNING",
        WatchdogState::Suspended => "SUSPENDED",
        WatchdogState::Recovering => "RECOVERING",
        WatchdogState::SafeMode => "SAFE MODE",
    };

    let header = Paragraph::new(vec![Line::from(vec![
        Span::styled(
            "  AIOS v0.5.0",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!("{}", state.ai_tier),
            match state.ai_tier {
                AiTier::Tier1 => Style::default().fg(Color::Green),
                AiTier::Tier2 => Style::default().fg(Color::Yellow),
                AiTier::Tier3 => Style::default().fg(Color::Red),
            },
        ),
        Span::raw("  |  WD: "),
        Span::styled(
            wd_label,
            Style::default().fg(wd_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  CPU: "),
        Span::styled(
            format!("{}", state.hardware.cpu.cores),
            Style::default().fg(Color::White),
        ),
        Span::raw("  |  RAM: "),
        Span::styled(
            format!("{}/{}MB", state.ram_used, state.ram_total),
            Style::default().fg(Color::White),
        ),
        Span::raw("  |  Blocks: "),
        Span::styled(
            format!("{}", state.blocks_count),
            Style::default().fg(Color::Green),
        ),
        Span::raw("  |  Proc: "),
        Span::styled(
            format!("{}", state.process_count),
            Style::default().fg(Color::Green),
        ),
    ])])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" AIOS Dashboard "),
    );
    f.render_widget(header, area);
}

fn draw_tabs(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let titles = vec![
        " Overview ",
        " Processes ",
        " Blocks ",
        " Metrics ",
        " Deps ",
        " Web ",
        " Shell ",
        " Files ",
    ];

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" Tabs "))
        .select(state.selected_tab)
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .divider("|")
        .padding(" ", " ");

    let tabs_area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0)])
        .split(area);

    f.render_widget(tabs, tabs_area[0]);
}

fn draw_main(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    match state.selected_tab {
        0 => draw_overview(f, area, state),
        1 => draw_processes(f, area, state),
        2 => draw_blocks(f, area, state),
        3 => draw_metrics(f, area, state),
        4 => draw_dependencies(f, area, state),
        5 => draw_web(f, area, state),
        6 => draw_shell(f, area, state),
        7 => draw_files(f, area, state),
        _ => draw_overview(f, area, state),
    }
}

fn draw_files(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    match &state.fm {
        Some(fm) => {
            let snap = fm.snapshot();
            let rows = area
                .height
                .saturating_sub(aios_fm::ui_tui::HEADER_HEIGHT + aios_fm::ui_tui::FOOTER_HEIGHT)
                as usize;
            aios_fm::ui_tui::draw(f, area, &snap, rows);
            if let Some(preview) = &state.fm_preview {
                draw_fm_preview(f, f.area(), preview);
            }
        }
        None => {
            let para = Paragraph::new(
                " File manager not initialized — start aios-tui with a tokio runtime. ",
            )
            .block(Block::default().borders(Borders::ALL).title(" Files "));
            f.render_widget(para, area);
        }
    }
}

fn draw_fm_preview(f: &mut Frame<'_>, area: Rect, preview: &AiPreview) {
    let title = format!(" {} ", preview.title);
    let lines: Vec<Line> = preview
        .lines
        .iter()
        .map(|(kind, text)| {
            let color = match kind {
                aios_vfs::ai_preview::AiLineKind::Info => Color::White,
                aios_vfs::ai_preview::AiLineKind::Success => Color::Green,
                aios_vfs::ai_preview::AiLineKind::Warning => Color::Yellow,
                aios_vfs::ai_preview::AiLineKind::Error => Color::Red,
                aios_vfs::ai_preview::AiLineKind::Muted => Color::DarkGray,
            };
            Line::from(Span::styled(text.as_str(), Style::default().fg(color)))
        })
        .collect();

    let height = (lines.len() + 2).min(area.height.saturating_sub(2) as usize) as u16;
    let width = 84.min(area.width.saturating_sub(2) as usize) as u16;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_alignment(ratatui::layout::Alignment::Left),
    );
    f.render_widget(Clear, area);
    f.render_widget(para, modal);
}

fn draw_overview(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    draw_hardware_info(f, chunks[0], state);
    draw_log_panel(f, chunks[1], state);
}

fn draw_hardware_info(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let items = vec![
        ListItem::new(Line::from(vec![Span::styled(
            "  Hardware",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )])),
        ListItem::new(Line::from(vec![
            Span::raw("    CPU: "),
            Span::styled(
                state.hardware.cpu.model.clone(),
                Style::default().fg(Color::White),
            ),
        ])),
        ListItem::new(Line::from(vec![
            Span::raw("    Cores: "),
            Span::styled(
                format!("{}", state.hardware.cpu.cores),
                Style::default().fg(Color::White),
            ),
            Span::raw("  Threads: "),
            Span::styled(
                format!("{}", state.hardware.cpu.threads),
                Style::default().fg(Color::White),
            ),
            Span::raw("  AVX2: "),
            Span::styled(
                format!("{}", state.hardware.cpu.has_avx2),
                Style::default().fg(if state.hardware.cpu.has_avx2 {
                    Color::Green
                } else {
                    Color::DarkGray
                }),
            ),
            Span::raw("  AVX-512: "),
            Span::styled(
                format!("{}", state.hardware.cpu.has_avx512),
                Style::default().fg(if state.hardware.cpu.has_avx512 {
                    Color::Green
                } else {
                    Color::DarkGray
                }),
            ),
        ])),
        ListItem::new(Line::from("")),
        ListItem::new(Line::from(vec![Span::styled(
            "  GPU",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )])),
        ListItem::new(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                state
                    .hardware
                    .gpu
                    .as_ref()
                    .map(|g| format!("{} ({}MB)", g.name, g.vram_mb))
                    .unwrap_or_else(|| "None".into()),
                Style::default().fg(Color::White),
            ),
        ])),
        ListItem::new(Line::from("")),
        ListItem::new(Line::from(vec![Span::styled(
            "  Storage",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )])),
        ListItem::new(if state.hardware.storage_devices.is_empty() {
            Line::from(vec![Span::styled(
                "    None detected",
                Style::default().fg(Color::DarkGray),
            )])
        } else {
            Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    format!("{} device(s)", state.hardware.storage_devices.len()),
                    Style::default().fg(Color::White),
                ),
            ])
        }),
        ListItem::new(Line::from("")),
        ListItem::new(Line::from(vec![Span::styled(
            "  System",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )])),
        ListItem::new(Line::from(vec![
            Span::raw("    Blocks: "),
            Span::styled(
                format!("{}", state.blocks_count),
                Style::default().fg(Color::Green),
            ),
            Span::raw("  Processes: "),
            Span::styled(
                format!("{}", state.process_count),
                Style::default().fg(Color::Green),
            ),
        ])),
        ListItem::new(Line::from(vec![
            Span::raw("    RAM: "),
            Span::styled(
                format!("{}MB / {}MB", state.ram_used, state.ram_total),
                Style::default().fg(Color::White),
            ),
        ])),
    ];

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" System Info "),
    );
    f.render_widget(list, area);
}

fn draw_processes(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(6)])
        .split(area);

    let header_cells = ["PID", "Name", "Priority", "State", "RAM", "CPU", "Crash"]
        .iter()
        .map(|h| {
            Cell::from(*h).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        });
    let header = Row::new(header_cells).height(1);

    let rows: Vec<Row> = state
        .processes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let style = if i == state.selected_row {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(format!("{}", p.pid)),
                Cell::from(p.name.clone()),
                Cell::from(p.priority.clone()).style(priority_style(&p.priority)),
                Cell::from(p.state.clone()).style(state_style(&p.state)),
                Cell::from(format!("{}MB", p.ram_mb)),
                Cell::from(format!("{}ms", p.cpu_ms)),
                Cell::from(format!("{}", p.crashes)).style(if p.crashes > 0 {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::Green)
                }),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(8),
        Constraint::Percentage(25),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Processes ({}) — j/k: navigate  K: kill ",
            state.processes.len()
        )))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    f.render_widget(table, chunks[0]);

    if let Some(ref result) = state.process_kill_result {
        let detail = Paragraph::new(Line::from(vec![Span::styled(
            format!("  {result}"),
            Style::default().fg(Color::Yellow),
        )]))
        .block(Block::default().borders(Borders::ALL).title(" Detail "));
        f.render_widget(detail, chunks[1]);
    } else if let Some(p) = state.processes.get(state.selected_row) {
        let lines = vec![
            Line::from(vec![
                Span::raw("  PID: "),
                Span::styled(format!("{}", p.pid), Style::default().fg(Color::White)),
                Span::raw("  Name: "),
                Span::styled(&p.name, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::raw("  Priority: "),
                Span::styled(&p.priority, priority_style(&p.priority)),
                Span::raw("  State: "),
                Span::styled(&p.state, state_style(&p.state)),
            ]),
            Line::from(vec![
                Span::raw("  RAM Quota: "),
                Span::styled(format!("{}MB", p.ram_mb), Style::default().fg(Color::White)),
                Span::raw("  CPU Time: "),
                Span::styled(format!("{}ms", p.cpu_ms), Style::default().fg(Color::White)),
                Span::raw("  Crashes: "),
                Span::styled(
                    format!("{}", p.crashes),
                    if p.crashes > 0 {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default().fg(Color::Green)
                    },
                ),
            ]),
        ];
        let detail = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Process Detail "),
        );
        f.render_widget(detail, chunks[1]);
    } else {
        let detail = Paragraph::new(Line::from(vec![Span::styled(
            "  No process selected",
            Style::default().fg(Color::DarkGray),
        )]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Process Detail "),
        );
        f.render_widget(detail, chunks[1]);
    }
}

fn draw_blocks(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(5)])
        .split(area);

    let header_cells = ["ID", "Name", "Version", "State", "Size"].iter().map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header = Row::new(header_cells).height(1);

    let rows: Vec<Row> = state
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let style = if i == state.selected_row {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(format!("{}", b.id)),
                Cell::from(b.name.clone()),
                Cell::from(b.version.clone()),
                Cell::from(b.state.clone()).style(block_state_style(&b.state)),
                Cell::from(format!("{}B", b.size)),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(8),
        Constraint::Percentage(25),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(12),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Blocks ({}) — j/k: navigate  U: unload  L: load  H: hot-swap ",
            state.blocks.len()
        )))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    f.render_widget(table, chunks[0]);

    match &state.block_input_mode {
        BlockInputMode::LoadName => {
            let input_line = Line::from(vec![
                Span::styled(
                    "  Block name: ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{}█", state.block_input_buffer),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    "  Enter: confirm  Esc: cancel",
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            let detail = Paragraph::new(input_line).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Load Block — Step 1/2 "),
            );
            f.render_widget(detail, chunks[1]);
        }
        BlockInputMode::LoadVersion => {
            let input_line = Line::from(vec![
                Span::styled(
                    "  Version: ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{}█", state.block_input_buffer),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    "  Enter: confirm  Esc: cancel",
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            let detail = Paragraph::new(input_line).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Load Block — Step 2/2 "),
            );
            f.render_widget(detail, chunks[1]);
        }
        BlockInputMode::None => {
            if let Some(ref result) = state.block_operation_result {
                let detail = Paragraph::new(Line::from(vec![Span::styled(
                    format!("  {result}"),
                    Style::default().fg(Color::Yellow),
                )]))
                .block(Block::default().borders(Borders::ALL).title(" Detail "));
                f.render_widget(detail, chunks[1]);
            } else if let Some((name, version)) = state.selected_block_name_version() {
                let lines = vec![
                    Line::from(vec![
                        Span::raw("  Name: "),
                        Span::styled(&name, Style::default().fg(Color::White)),
                        Span::raw("  Version: "),
                        Span::styled(&version, Style::default().fg(Color::White)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            "  U",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" — Unload block  "),
                        Span::styled(
                            "L",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" — Load from disk  "),
                        Span::styled(
                            "H",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" — Hot-swap binary"),
                    ]),
                ];
                let detail = Paragraph::new(lines).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Block Detail "),
                );
                f.render_widget(detail, chunks[1]);
            } else {
                let detail = Paragraph::new(Line::from(vec![Span::styled(
                    "  No block selected",
                    Style::default().fg(Color::DarkGray),
                )]))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Block Detail "),
                );
                f.render_widget(detail, chunks[1]);
            }
        }
    }
}

fn draw_metrics(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Min(5),
        ])
        .split(area);

    let ram_pct = if state.ram_total > 0 {
        state.ram_used as f64 / state.ram_total as f64
    } else {
        0.0
    };
    let ram_color = if ram_pct > 0.85 {
        Color::Red
    } else if ram_pct > 0.6 {
        Color::Yellow
    } else {
        Color::Green
    };

    let ram_gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(" RAM Usage "))
        .gauge_style(
            Style::default()
                .fg(ram_color)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .ratio(ram_pct)
        .label(format!(
            "{}MB / {}MB ({:.1}%)",
            state.ram_used,
            state.ram_total,
            ram_pct * 100.0
        ));

    f.render_widget(ram_gauge, chunks[0]);

    let mut pri_counts = [0u32; 5];
    for p in &state.processes {
        match p.priority.as_str() {
            "Background" => pri_counts[0] += 1,
            "Low" => pri_counts[1] += 1,
            "Normal" => pri_counts[2] += 1,
            "High" => pri_counts[3] += 1,
            "Critical" => pri_counts[4] += 1,
            _ => {}
        }
    }

    let pri_items = vec![
        ListItem::new(Line::from(vec![
            Span::styled(
                "  Critical",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(": "),
            Span::styled(
                format!("{}", pri_counts[4]),
                Style::default().fg(Color::White),
            ),
            Span::raw("  "),
            Span::raw("█".repeat((pri_counts[4] * 4) as usize)),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled(
                "  High    ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": "),
            Span::styled(
                format!("{}", pri_counts[3]),
                Style::default().fg(Color::White),
            ),
            Span::raw("  "),
            Span::raw("█".repeat((pri_counts[3] * 4) as usize)),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled(
                "  Normal  ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": "),
            Span::styled(
                format!("{}", pri_counts[2]),
                Style::default().fg(Color::White),
            ),
            Span::raw("  "),
            Span::raw("█".repeat((pri_counts[2] * 4) as usize)),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled(
                "  Low     ",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": "),
            Span::styled(
                format!("{}", pri_counts[1]),
                Style::default().fg(Color::White),
            ),
            Span::raw("  "),
            Span::raw("█".repeat((pri_counts[1] * 4) as usize)),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled(
                "  Bg      ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": "),
            Span::styled(
                format!("{}", pri_counts[0]),
                Style::default().fg(Color::White),
            ),
            Span::raw("  "),
            Span::raw("█".repeat((pri_counts[0] * 4) as usize)),
        ])),
    ];

    let pri_list = List::new(pri_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Process Priority Distribution "),
    );
    f.render_widget(pri_list, chunks[1]);

    let history_items: Vec<ListItem> = state
        .ram_history
        .iter()
        .rev()
        .take(20)
        .enumerate()
        .map(|(i, &val)| {
            let bar_len = if state.ram_total > 0 {
                (val as usize * 40 / state.ram_total as usize).min(40)
            } else {
                0
            };
            let bar_color = if state.ram_total > 0 {
                let pct = val as f64 / state.ram_total as f64;
                if pct > 0.85 {
                    Color::Red
                } else if pct > 0.6 {
                    Color::Yellow
                } else {
                    Color::Green
                }
            } else {
                Color::Green
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {:>5}MB ", val),
                    Style::default().fg(Color::White),
                ),
                Span::styled("█".repeat(bar_len), Style::default().fg(bar_color)),
                Span::styled(
                    "░".repeat(40 - bar_len),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("  -{}s", i), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let history_list = List::new(history_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" RAM History (recent) "),
    );
    f.render_widget(history_list, chunks[2]);
}

fn draw_log_panel(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let items: Vec<ListItem> = state
        .log_messages
        .iter()
        .rev()
        .take(20)
        .map(|msg| {
            let color = if msg.contains("error") || msg.contains("crash") || msg.contains("Kill") {
                Color::Red
            } else if msg.contains("warn") {
                Color::Yellow
            } else if msg.contains("success") || msg.contains("OK") || msg.contains("loaded") {
                Color::Green
            } else {
                Color::White
            };
            ListItem::new(Line::from(Span::styled(
                format!("  {msg}"),
                Style::default().fg(color),
            )))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Activity Log "),
    );
    f.render_widget(list, area);
}

fn draw_dependencies(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(5)])
        .split(area);

    let header_cells = ["#", "Block", "Depends On", "Depended By"].iter().map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header = Row::new(header_cells).height(1);

    let rows: Vec<Row> = state
        .dep_snapshot
        .blocks
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let style = if i == state.selected_row {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let deps_str = {
                let block = state.blocks.iter().find(|b| &b.name == name);
                match block {
                    Some(b) if !b.deps.is_empty() => b.deps.join(", "),
                    _ => String::from("--"),
                }
            };
            let dependents_str = {
                let block = state.blocks.iter().find(|b| &b.name == name);
                match block {
                    Some(b) if !b.dependents.is_empty() => b.dependents.join(", "),
                    _ => String::from("--"),
                }
            };
            Row::new(vec![
                Cell::from(format!("{}", i + 1)),
                Cell::from(name.clone()).style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(deps_str).style(Style::default().fg(Color::Green)),
                Cell::from(dependents_str).style(Style::default().fg(Color::Magenta)),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Percentage(25),
        Constraint::Percentage(30),
        Constraint::Percentage(30),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Dependency Graph ({}) ",
            state.dep_snapshot.blocks.len()
        )))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    f.render_widget(table, chunks[0]);

    let load_order_str = if state.dep_snapshot.load_order.is_empty() {
        "  No blocks registered".to_string()
    } else {
        format!(
            "  Load order: {}",
            state.dep_snapshot.load_order.join(" -> ")
        )
    };
    let edge_count = state.dep_snapshot.edges.len();

    let summary = Paragraph::new(vec![
        Line::from(load_order_str).style(Style::default().fg(Color::White)),
        Line::from(vec![
            Span::raw("  Edges: "),
            Span::styled(format!("{}", edge_count), Style::default().fg(Color::Green)),
            Span::raw("  Blocks: "),
            Span::styled(
                format!("{}", state.dep_snapshot.blocks.len()),
                Style::default().fg(Color::Green),
            ),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Load Order & Stats "),
    );
    f.render_widget(summary, chunks[1]);
}

fn draw_web_sidebar(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let ws = &state.web_state;
    let entries = web_nav_entries(ws);
    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let selected = i == ws.history_sel;
            let base = if e.is_current {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Blue)
            };
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                base
            };
            let glyph = if e.is_current { "▸" } else { " " };
            ListItem::new(Line::from(vec![
                Span::styled(glyph, Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {}", e.label), style),
            ]))
        })
        .collect();
    let title = if ws.sidebar_focused {
        " Nav — j/k:sel  Enter:go  Esc:back "
    } else {
        " Nav — \\:focus "
    };
    let sidebar = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(sidebar, area);
}

fn draw_web(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let ws = &state.web_state;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);

    let sidebar_width = if area.width as usize > SIDEBAR_WIDTH + 10 {
        SIDEBAR_WIDTH as u16
    } else {
        0
    };
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(1)])
        .split(chunks[1]);
    draw_web_sidebar(f, body[0], state);
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(8)])
        .split(body[1]);

    let url_style = if ws.input_focused {
        Style::default().fg(Color::Black).bg(Color::White)
    } else {
        Style::default().fg(Color::White)
    };
    let url_display = if ws.input_focused {
        format!("{}{}", ws.url_input, "█")
    } else if !ws.url_input.is_empty() {
        ws.url_input.clone()
    } else if !ws.search_query.is_empty() {
        format!("search: {}", ws.search_query)
    } else if !ws.current_url.is_empty() {
        ws.current_url.clone()
    } else {
        "type a search query or a URL".into()
    };
    let url_bar = Paragraph::new(Line::from(Span::styled(
        format!("  {}  ", url_display),
        url_style,
    )))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Omnibox — g:focus Enter:go "),
    );
    f.render_widget(url_bar, chunks[0]);

    if let Some(ref err) = ws.error {
        let err_para = Paragraph::new(Line::from(Span::styled(
            format!("  Error: {err}"),
            Style::default().fg(Color::Red),
        )))
        .block(Block::default().borders(Borders::ALL).title(" Error "));
        f.render_widget(err_para, main[0]);
    } else if ws.loading {
        let loading = Paragraph::new(Line::from(Span::styled(
            "  Loading...",
            Style::default().fg(Color::Yellow),
        )))
        .block(Block::default().borders(Borders::ALL).title(" Loading "));
        f.render_widget(loading, main[0]);
    } else if let Some(ref page) = ws.page {
        let visible = main[0].height.saturating_sub(2) as usize;
        let wrapped = wrap_text(&page.text, ws.wrap_width);
        let lines: Vec<Line> = wrapped
            .iter()
            .skip(ws.scroll)
            .take(visible)
            .map(|l| {
                let trimmed = l.trim_start();
                let style = if trimmed.starts_with('#') {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if trimmed.is_empty() {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(format!("  {l}"), style))
            })
            .collect();
        let total = wrapped.len();
        let scroll_hint = if total > visible {
            format!(
                "  — {} links · u/d {}–{} · B:browser  ",
                page.links.len(),
                ws.scroll,
                ws.scroll.saturating_add(visible)
            )
        } else {
            format!("  — {} links · B:browser  ", page.links.len())
        };
        let content = Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} — {}", page.title, scroll_hint)),
            );
        f.render_widget(content, main[0]);
    } else {
        let placeholder = Paragraph::new(vec![
            Line::from(Span::styled(
                "  Press g or type a search query / URL and press Enter",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  Examples:",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "    how does AIOS work",
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                "    example.com",
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                "    https://duckduckgo.com",
                Style::default().fg(Color::White),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Web Browser "),
        );
        f.render_widget(placeholder, main[0]);
    }

    let link_items: Vec<ListItem> = ws
        .page
        .as_ref()
        .map(|page| {
            page.links
                .iter()
                .enumerate()
                .skip(ws.links_scroll)
                .take(LINKS_VIEW_ROWS)
                .map(|(i, (text, href))| {
                    let style = if i == state.selected_row {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Blue)
                    };
                    let display = if text.is_empty() { href } else { text };
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{:>3} ", i + 1),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(display.to_string(), style),
                        Span::styled(format!("  → {href}"), Style::default().fg(Color::DarkGray)),
                    ]))
                })
                .collect()
        })
        .unwrap_or_default();

    let link_range = ws
        .page
        .as_ref()
        .map(|p| {
            if p.links.is_empty() {
                "0–0".to_string()
            } else {
                let end = (ws.links_scroll + LINKS_VIEW_ROWS).min(p.links.len());
                format!("{}–{}", ws.links_scroll + 1, end)
            }
        })
        .unwrap_or_default();
    let link_list = List::new(link_items).block(Block::default().borders(Borders::ALL).title(
        format!(" Links — o:open  j/k:sel  b:back  n:browser  {link_range} "),
    ));
    f.render_widget(link_list, main[1]);
}

fn draw_shell(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(area);

    let output_lines: Vec<Line> = state
        .shell_state
        .output
        .iter()
        .rev()
        .take(area.height.saturating_sub(4) as usize)
        .rev()
        .map(|l| Line::from(Span::raw(format!("  {l}"))))
        .collect();

    let output = Paragraph::new(output_lines).block(Block::default().borders(Borders::ALL).title(
        format!(" Shell — {} lines ", state.shell_state.output.len()),
    ));
    f.render_widget(output, chunks[0]);

    let input_display = format!("  $ {}{}", state.shell_state.input_buffer, "█");
    let input = Paragraph::new(Line::from(Span::styled(
        input_display,
        Style::default().fg(Color::Yellow),
    )))
    .block(Block::default().borders(Borders::ALL).title(" Input "));
    f.render_widget(input, chunks[1]);
}

fn draw_help(f: &mut Frame<'_>, area: Rect) {
    f.render_widget(Clear, area);

    let mut help_text =
        vec![
        Line::from(Span::styled(
            " AIOS TUI Help — press F1 or Esc to close ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            " Keyboard Shortcuts:",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("  F1 / ?     — Toggle this help screen")),
        Line::from(Span::raw("  q / Esc    — Quit AIOS")),
        Line::from(Span::raw(
            "  1-8        — Switch tabs (Overview/Processes/Blocks/Metrics/Deps/Web/Shell/Files)",
        )),
        Line::from(Span::raw("  j / Down   — Move selection down")),
        Line::from(Span::raw("  k / Up     — Move selection up")),
        Line::from(Span::raw("  r          — Refresh")),
        Line::from(Span::raw("  s          — Record telemetry snapshot")),
        Line::from(Span::raw("  W          — Open AIOS GUI dashboard")),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            " Process Tab (1):",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("  K          — Kill selected process")),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            " Blocks Tab (2):",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw(
            "  L          — Load block (enter name, then version)",
        )),
        Line::from(Span::raw("  H          — Hot-swap block from disk")),
        Line::from(Span::raw("  U          — Unload selected block")),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            " Web Tab (6):",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw(
            "  g          — Focus omnibox (search query or URL)",
        )),
        Line::from(Span::raw("  Enter      — Search / navigate in omnibox")),
        Line::from(Span::raw("  o / Enter  — Open selected link")),
        Line::from(Span::raw("  j/k        — Navigate links")),
        Line::from(Span::raw("  u/d        — Scroll page content")),
        Line::from(Span::raw("  b          — Go back in history")),
        Line::from(Span::raw("  Esc        — Unfocus omnibox")),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            " Shell Tab (7):",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("  Type command and press Enter")),
        Line::from(Span::raw("  ↑/↓       — Command history navigation")),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            " Files Tab (8):",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("  Tab / j/k — Switch / navigate panels")),
        Line::from(Span::raw("  Enter      — Open directory / AI-preview file")),
        Line::from(Span::raw("  Backspace  — Go to parent directory")),
        Line::from(Span::raw("  F3 / o     — AI-preview selected file")),
        Line::from(Span::raw("  F5         — Copy to other panel")),
        Line::from(Span::raw("  F6         — Move to other panel")),
        Line::from(Span::raw("  F7         — Create directory (input modal)")),
        Line::from(Span::raw("  F8         — Delete selected item")),
        Line::from(Span::raw("  F2         — Rename selected item (input modal)")),
        Line::from(Span::raw("  F9 / s     — Cycle sort rule")),
        Line::from(Span::raw("  g / w      — Grant HOST:// read / write capability")),
        Line::from(Span::raw("  r          — Refresh panels")),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            " Shell Commands:",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("  ps           — List processes")),
        Line::from(Span::raw("  blocks       — List loaded blocks")),
        Line::from(Span::raw("  spawn <n> [p] [m] — Spawn process")),
        Line::from(Span::raw("  kill <pid>   — Kill process")),
        Line::from(Span::raw("  load <name> [ver]  — Load block")),
        Line::from(Span::raw("  unload <id>  — Unload block")),
        Line::from(Span::raw("  status       — System status")),
        Line::from(Span::raw("  fetch <url>  — Download & load block from URL")),
        Line::from(Span::raw("  search <q>   — Web search (via DuckDuckGo)")),
        Line::from(Span::raw("  logs         — View safe mode logs")),
        Line::from(Span::raw("  restart      — Restart orchestrator")),
        Line::from(Span::raw("  help         — Show shell commands")),
        Line::from(Span::raw("  clear        — Clear output")),
    ];
    while (help_text.len() as u16) < area.height.saturating_sub(2) {
        help_text.push(Line::from(""));
    }

    let help_para = Paragraph::new(help_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Help "),
        )
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(help_para, area);
}

fn draw_footer(f: &mut Frame<'_>, area: Rect) {
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            " q",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("=Quit  "),
        Span::styled(
            "1-7",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("=Tab  "),
        Span::styled(
            "j/k",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("=Nav  "),
        Span::styled(
            "F1",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("=Help  "),
        Span::styled(
            "K",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("=Kill  "),
        Span::styled(
            ":",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("=Cmd"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, area);
}

fn priority_style(pri: &str) -> Style {
    match pri {
        "Critical" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "High" => Style::default().fg(Color::Yellow),
        "Normal" => Style::default().fg(Color::Green),
        "Low" => Style::default().fg(Color::Blue),
        "Background" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::White),
    }
}

fn state_style(st: &str) -> Style {
    match st {
        "Running" => Style::default().fg(Color::Green),
        "Ready" => Style::default().fg(Color::Cyan),
        "Suspended" => Style::default().fg(Color::Yellow),
        "Terminated" => Style::default().fg(Color::DarkGray),
        "Crashed" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::White),
    }
}

fn block_state_style(st: &str) -> Style {
    match st {
        "Active" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "Loaded" => Style::default().fg(Color::Cyan),
        "Frozen" => Style::default().fg(Color::Yellow),
        "Unloaded" => Style::default().fg(Color::DarkGray),
        "Error" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::White),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_block_mgr::registry::BlockRegistry;
    use aios_process_mgr::scheduler::Scheduler;

    fn make_state() -> DashboardState {
        let profile = HardwareProfile::mock_modern();
        let mut reg = BlockRegistry::new();
        reg.register_block("test", "0.1.0", b"test".to_vec())
            .unwrap();
        let sched = Scheduler::new(65536);
        DashboardState::new(AiTier::Tier1, profile, &reg, &sched)
    }

    #[test]
    fn test_dashboard_state_creation() {
        let state = make_state();
        assert_eq!(state.blocks_count, 1);
        assert_eq!(state.process_count, 0);
        assert_eq!(state.ram_total, 65536);
        assert_eq!(state.watchdog_state, WatchdogState::Monitoring);
        assert_eq!(state.selected_tab, 0);
        assert_eq!(state.selected_row, 0);
    }

    #[test]
    fn test_add_log() {
        let mut state = make_state();
        state.add_log("test message".into());
        assert_eq!(state.log_messages.len(), 4);
    }

    #[test]
    fn test_add_log_limit() {
        let mut state = make_state();
        for i in 0..110 {
            state.add_log(format!("msg {i}"));
        }
        assert_eq!(state.log_messages.len(), 100);
    }

    #[test]
    fn test_update_watchdog() {
        let mut state = make_state();
        state.update_watchdog(WatchdogState::SafeMode);
        assert_eq!(state.watchdog_state, WatchdogState::SafeMode);
    }

    #[test]
    fn test_move_selection() {
        let mut state = make_state();
        state.selected_tab = 1;
        state.move_selection_down();
        assert_eq!(state.selected_row, 0);
        state.move_selection_up();
        assert_eq!(state.selected_row, 0);
    }

    #[test]
    fn test_selected_process_pid() {
        let state = make_state();
        assert!(state.selected_process_pid().is_none());
    }

    #[test]
    fn test_priority_styles() {
        assert_eq!(priority_style("Critical").fg, Some(Color::Red));
        assert_eq!(priority_style("High").fg, Some(Color::Yellow));
        assert_eq!(priority_style("Normal").fg, Some(Color::Green));
        assert_eq!(priority_style("Low").fg, Some(Color::Blue));
        assert_eq!(priority_style("Background").fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_state_styles() {
        assert_eq!(state_style("Running").fg, Some(Color::Green));
        assert_eq!(state_style("Crashed").fg, Some(Color::Red));
        assert_eq!(state_style("Terminated").fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_block_state_styles() {
        assert_eq!(block_state_style("Active").fg, Some(Color::Green));
        assert_eq!(block_state_style("Error").fg, Some(Color::Red));
    }

    fn make_page(url: &str, links: usize) -> PageContent {
        PageContent {
            url: url.to_string(),
            title: format!("Title {url}"),
            text: "text".into(),
            links: (0..links)
                .map(|i| (format!("t{i}"), format!("{url}/{i}")))
                .collect(),
        }
    }

    #[test]
    fn test_web_cache_insert_and_lookup() {
        let mut ws = WebState {
            url_input: String::new(),
            current_url: String::new(),
            search_query: String::new(),
            page: None,
            loading: false,
            error: None,
            input_focused: false,
            scroll: 0,
            links_scroll: 0,
            history: Vec::new(),
            cache: Vec::new(),
            web_fetch_gen: 0,
            wrap_width: 78,
            sidebar_focused: false,
            history_sel: 0,
        };
        assert!(ws.cached_page("https://a").is_none());
        ws.cache_page(make_page("https://a", 0));
        ws.cache_page(make_page("https://b", 0));
        assert!(ws.cached_page("https://a").is_some());
        assert_eq!(ws.cached_page("https://b").unwrap().url, "https://b");
        ws.cache_page(make_page("https://a", 1));
        assert_eq!(ws.cache.len(), 2);
    }

    #[test]
    fn test_web_cache_eviction_caps_at_bound() {
        let mut ws = WebState {
            url_input: String::new(),
            current_url: String::new(),
            search_query: String::new(),
            page: None,
            loading: false,
            error: None,
            input_focused: false,
            scroll: 0,
            links_scroll: 0,
            history: Vec::new(),
            cache: Vec::new(),
            web_fetch_gen: 0,
            wrap_width: 78,
            sidebar_focused: false,
            history_sel: 0,
        };
        for i in 0..WEB_CACHE_CAP + 5 {
            ws.cache_page(make_page(&format!("https://e{i}"), 0));
        }
        assert_eq!(ws.cache.len(), WEB_CACHE_CAP);
        assert!(ws.cached_page("https://e0").is_none());
        assert!(ws
            .cached_page(&format!("https://e{}", WEB_CACHE_CAP + 4))
            .is_some());
    }

    #[test]
    fn test_web_links_scroll_keeps_selection_visible() {
        let mut state = make_state();
        state.selected_tab = 5;
        state.web_state.page = Some(make_page("https://p", 12));
        for _ in 0..12 {
            state.move_selection_down();
        }
        assert_eq!(state.selected_row, 11);
        assert_eq!(
            state.web_state.links_scroll,
            12usize.saturating_sub(LINKS_VIEW_ROWS)
        );
        for _ in 0..12 {
            state.move_selection_up();
        }
        assert_eq!(state.selected_row, 0);
        assert_eq!(state.web_state.links_scroll, 0);
    }

    #[test]
    fn test_web_fetch_result_applied() {
        let mut state = make_state();
        state.selected_tab = 5;
        state.web_state.web_fetch_gen = 3;
        let page = make_page("https://applied", 0);
        *state.page_cache.lock().unwrap() = Some((3, Ok((page, Some("q".into())))));
        state.check_page_cache();
        assert!(state.web_state.page.is_some());
        assert_eq!(state.web_state.current_url, "https://applied");
        assert_eq!(state.web_state.search_query, "q");
        assert!(!state.web_state.loading);
        assert!(state.web_state.error.is_none());
        assert_eq!(state.web_state.cache.len(), 1);
    }

    #[test]
    fn test_web_stale_generation_dropped() {
        let mut state = make_state();
        state.web_state.web_fetch_gen = 3;
        let page = make_page("https://stale", 0);
        *state.page_cache.lock().unwrap() = Some((2, Ok((page, None))));
        state.check_page_cache();
        assert!(state.web_state.page.is_none());
        assert_eq!(state.web_state.cache.len(), 0);
    }

    #[test]
    fn test_wrap_text_short_lines_unchanged() {
        let wrapped = wrap_text("hello world", 80);
        assert_eq!(wrapped, vec!["hello world".to_string()]);
    }

    #[test]
    fn test_wrap_text_splits_at_word_boundaries() {
        let wrapped = wrap_text("aaa bbb ccc ddd", 7);
        assert_eq!(wrapped, vec!["aaa bbb".to_string(), "ccc ddd".to_string()]);
    }

    #[test]
    fn test_wrap_text_hard_splits_long_words() {
        let wrapped = wrap_text("abcdefghijkl", 4);
        assert_eq!(
            wrapped,
            vec!["abcd".to_string(), "efgh".to_string(), "ijkl".to_string(),]
        );
    }

    #[test]
    fn test_wrap_text_preserves_indent_and_blanks() {
        let text = "\n  • item with long words here\n\nplain\n";
        let wrapped = wrap_text(text, 12);
        assert_eq!(
            wrapped,
            vec![
                String::new(),
                "  • item".to_string(),
                "with long".to_string(),
                "words here".to_string(),
                String::new(),
                "plain".to_string(),
            ]
        );
    }

    #[test]
    fn test_web_page_width_accounts_for_sidebar() {
        assert_eq!(web_page_width(80), 80 - SIDEBAR_WIDTH - 4);
        assert_eq!(web_page_width(120), 120 - SIDEBAR_WIDTH - 4);
    }

    #[test]
    fn test_web_page_width_clamps_low() {
        assert_eq!(web_page_width(0), 4);
        assert_eq!(web_page_width(5), 4);
    }

    #[test]
    fn test_compact_url_label_strips_scheme_and_www() {
        assert_eq!(
            compact_url_label("https://www.example.com/path", 30),
            "example.com/path"
        );
        assert_eq!(compact_url_label("http://example.com", 30), "example.com");
    }

    #[test]
    fn test_compact_url_label_truncates() {
        let label = compact_url_label("https://www.verylongdomainname.com/deep/path", 12);
        assert_eq!(label.chars().count(), 12);
        assert!(label.ends_with('…'));
        assert!(label.starts_with("verylongdom"));
    }

    #[test]
    fn test_web_nav_entries_current_first_history_newest() {
        let mut state = make_state();
        state.web_state.current_url = "https://b".into();
        state.web_state.history = vec!["https://a".into(), "https://b".into(), "https://c".into()];
        let entries = web_nav_entries(&state.web_state);
        let urls: Vec<&str> = entries.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(urls, vec!["https://b", "https://c", "https://a"]);
        assert!(entries[0].is_current);
        assert!(!entries[1].is_current);
    }

    #[test]
    fn test_web_nav_entries_empty_when_no_history() {
        let state = make_state();
        let entries = web_nav_entries(&state.web_state);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_web_nav_entries_dedups_current() {
        let mut state = make_state();
        state.web_state.current_url = "https://b".into();
        state.web_state.history = vec!["https://a".into(), "https://b".into()];
        let entries = web_nav_entries(&state.web_state);
        let urls: Vec<&str> = entries.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(urls, vec!["https://b", "https://a"]);
    }
}
