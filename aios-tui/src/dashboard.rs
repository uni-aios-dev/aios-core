use std::sync::{Arc, Mutex};

use aios_block_mgr::registry::BlockRegistry;
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_process_mgr::scheduler::Scheduler;
use aios_watchdog::watchdog::WatchdogState;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Gauge, List, ListItem, Paragraph, Row, Table, Tabs},
    Frame,
};

#[derive(Clone, Debug)]
pub struct PageContent {
    pub url: String,
    pub title: String,
    pub text: String,
    pub links: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct WebState {
    pub url_input: String,
    pub current_url: String,
    pub page: Option<PageContent>,
    pub loading: bool,
    pub error: Option<String>,
    pub input_focused: bool,
    pub scroll: usize,
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
    pub page_cache: Arc<Mutex<Option<PageContent>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockInputMode {
    None,
    LoadName,
    LoadVersion,
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
                page: None,
                loading: false,
                error: None,
                input_focused: false,
                scroll: 0,
            },
            page_cache: Arc::new(Mutex::new(None)),
        }
    }

    pub fn add_log(&mut self, msg: String) {
        self.log_messages.push(msg);
        if self.log_messages.len() > 100 {
            self.log_messages.remove(0);
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
            _ => 0,
        };
        if max > 0 && self.selected_row < max - 1 {
            self.selected_row += 1;
        }
    }

    pub fn check_page_cache(&mut self) {
        let content = self.page_cache.lock().ok().and_then(|mut c| c.take());
        if let Some(page) = content {
            let url = page.url.clone();
            self.web_state.page = Some(page);
            self.web_state.current_url = url.clone();
            self.web_state.loading = false;
            self.web_state.error = None;
            self.web_state.scroll = 0;
            self.add_log(format!("Web: loaded {}", url));
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
        _ => draw_overview(f, area, state),
    }
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

fn draw_web(f: &mut Frame<'_>, area: Rect, state: &DashboardState) {
    let ws = &state.web_state;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(8),
        ])
        .split(area);

    let url_style = if ws.input_focused {
        Style::default().fg(Color::Black).bg(Color::White)
    } else {
        Style::default().fg(Color::White)
    };
    let url_display = if ws.url_input.is_empty() && !ws.input_focused {
        if !ws.current_url.is_empty() {
            ws.current_url.clone()
        } else {
            "https://example.com".into()
        }
    } else {
        format!(
            "{}{}",
            ws.url_input,
            if ws.input_focused { "█" } else { "" }
        )
    };
    let url_bar = Paragraph::new(Line::from(Span::styled(
        format!("  {}  ", url_display),
        url_style,
    )))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" URL — g:focus Enter:go "),
    );
    f.render_widget(url_bar, chunks[0]);

    if let Some(ref err) = ws.error {
        let err_para = Paragraph::new(Line::from(Span::styled(
            format!("  Error: {err}"),
            Style::default().fg(Color::Red),
        )))
        .block(Block::default().borders(Borders::ALL).title(" Error "));
        f.render_widget(err_para, chunks[1]);
    } else if ws.loading {
        let loading = Paragraph::new(Line::from(Span::styled(
            "  Loading...",
            Style::default().fg(Color::Yellow),
        )))
        .block(Block::default().borders(Borders::ALL).title(" Loading "));
        f.render_widget(loading, chunks[1]);
    } else if let Some(ref page) = ws.page {
        let lines: Vec<Line> = page
            .text
            .lines()
            .skip(ws.scroll)
            .take(20)
            .map(|l| {
                Line::from(Span::styled(
                    format!("  {l}"),
                    Style::default().fg(Color::White),
                ))
            })
            .collect();
        let content =
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(format!(
                " {} — {} links ",
                page.title,
                page.links.len()
            )));
        f.render_widget(content, chunks[1]);
    } else {
        let placeholder = Paragraph::new(vec![
            Line::from(Span::styled(
                "  Enter a URL and press Enter",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  Examples:",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "    https://example.com",
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
        f.render_widget(placeholder, chunks[1]);
    }

    let link_items: Vec<ListItem> = ws
        .page
        .as_ref()
        .map(|page| {
            page.links
                .iter()
                .enumerate()
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

    let link_list = List::new(link_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Links — o: open selected j/k: navigate "),
    );
    f.render_widget(link_list, chunks[2]);
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
            "1-6",
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
            "K",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("=Kill  "),
        Span::styled(
            "g",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("=URL  "),
        Span::styled(
            "o",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("=Open"),
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
}
