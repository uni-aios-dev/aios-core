use crate::tui::app_state::TuiApp;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs};
use ratatui::Frame;

const TITLES: &[&str] = &[
    " System & HW ",
    " Blocks & Svc ",
    " AI Console ",
    " Studio GUI ",
];

pub fn draw(frame: &mut Frame, app: &mut TuiApp) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(10),
            Constraint::Length(6),
        ])
        .split(area);

    draw_header(frame, chunks[0], app);
    draw_tabs(frame, chunks[1], app);
    match app.current_tab {
        0 => draw_system_tab(frame, chunks[2], app),
        1 => draw_blocks_tab(frame, chunks[2], app),
        2 => draw_ai_tab(frame, chunks[2], app),
        3 => draw_bridge_tab(frame, chunks[2], app),
        _ => {}
    }
    draw_logs(frame, chunks[3], app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let state = app.state.lock().unwrap();
    let s = state.start_time.elapsed().as_secs();
    let uptime = format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60);

    let status_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let header = Line::from(vec![
        Span::styled(
            " AIOS v1.0.0 ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ Status: "),
        Span::styled("OK", status_style),
        Span::raw(" │ Uptime: "),
        Span::styled(uptime, Style::default().fg(Color::Yellow)),
        Span::raw(" │ CPU: "),
        Span::styled(
            state.hw_profile.cpu.brand.split(' ').next().unwrap_or("?"),
            Style::default().fg(Color::Magenta),
        ),
        Span::raw(" │ RAM: "),
        Span::styled(
            format!("{:.1}G", state.hw_profile.memory.total_gb),
            Style::default().fg(Color::Magenta),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::White));
    let paragraph = Paragraph::new(header).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let titles: Vec<Line> = TITLES
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i == app.current_tab {
                Line::from(Span::styled(
                    *t,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                ))
            } else {
                Line::from(Span::styled(*t, Style::default().fg(Color::White)))
            }
        })
        .collect();
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" Navigation "))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, area);
}

fn draw_system_tab(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let state = app.state.lock().unwrap();
    let hw = &state.hw_profile;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let cpu_lines = vec![
        Line::from(format!("CPU: {}", hw.cpu.brand)),
        Line::from(format!("Architecture: {}", hw.cpu.architecture)),
        Line::from(format!(
            "Physical cores: {}  Logical cores: {}",
            hw.cpu.physical_cores, hw.cpu.logical_cores
        )),
        Line::from(format!("Flags: {}", hw.cpu.flags.join(", "))),
        Line::from(format!(
            "RAM: {:.1} GB total  {:.1} GB used  {:.1} GB free",
            hw.memory.total_gb,
            hw.memory.used_gb,
            hw.memory.free_bytes as f64 / 1_073_741_824.0
        )),
    ];
    let cpu_block = Block::default()
        .title(" CPU & Memory ")
        .borders(Borders::ALL);
    let cpu_para = Paragraph::new(Text::from(cpu_lines)).block(cpu_block);
    frame.render_widget(cpu_para, chunks[0]);

    let os_lines = vec![
        Line::from(format!("OS: {} {}", hw.os.name, hw.os.os_version)),
        Line::from(format!("Kernel: {}", hw.os.kernel_version)),
        Line::from(format!("Hostname: {}", hw.os.hostname)),
        Line::from(format!("Uptime: {}s", hw.os.uptime_secs)),
        Line::from(format!(
            "Subsystems: Bridge={}, IPC=Active, LLM=Ready, WASM=Ready",
            if state
                .bridge_running
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                "Online"
            } else {
                "Starting"
            }
        )),
    ];
    let mut os_lines = {
        let mut lines = os_lines;
        if let Some(ref gpu) = hw.gpu {
            let gpu_name = if gpu.vram_gb > 0.0 {
                format!("GPU: {} ({:.1} GB VRAM)", gpu.model, gpu.vram_gb)
            } else {
                format!("GPU: {}", gpu.model)
            };
            lines.push(Line::from(gpu_name));
        }
        lines
    };
    os_lines.push(Line::from(format!("AI Tier: {}", hw.ai_tier)));

    let os_block = Block::default()
        .title(" System & AI ")
        .borders(Borders::ALL);
    let os_para = Paragraph::new(Text::from(os_lines)).block(os_block);
    frame.render_widget(os_para, chunks[1]);

    if hw.memory.total_gb > 0.0 {
        let ram_ratio = hw.memory.used_gb / hw.memory.total_gb;
        let gauge = Gauge::default()
            .block(Block::default().title(" RAM Usage "))
            .gauge_style(Style::default().fg(if ram_ratio > 0.8 {
                Color::Red
            } else if ram_ratio > 0.5 {
                Color::Yellow
            } else {
                Color::Green
            }))
            .percent((ram_ratio * 100.0) as u16);
        // overlay gauge on cpu section
        let gauge_area = Rect::new(
            chunks[0].x + 2,
            chunks[0].y + chunks[0].height - 3,
            chunks[0].width - 4,
            3,
        );
        if gauge_area.width > 10 {
            frame.render_widget(gauge, gauge_area);
        }
    }
}

fn draw_blocks_tab(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let state = app.state.lock().unwrap();
    let registry = state.bridge.registry.lock().unwrap();
    let scheduler = state.bridge.scheduler.lock().unwrap();

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let block_ids = registry.all_ids();
    let mut block_items: Vec<ListItem> = block_ids
        .iter()
        .map(|id| match registry.get(*id) {
            Ok(entry) => {
                let status = match entry.state {
                    aios_core::block::BlockState::Active => "Active",
                    aios_core::block::BlockState::Loaded => "Loaded",
                    aios_core::block::BlockState::Error => "Error",
                    _ => "Other",
                };
                ListItem::new(Line::from(format!(
                    "[{}] {} v{} [{}]",
                    id, entry.manifest.name, entry.manifest.version, status
                )))
            }
            Err(_) => ListItem::new(Line::from(format!("[{}] error", id))),
        })
        .collect();
    if block_items.is_empty() {
        block_items.push(ListItem::new(Line::from("No blocks loaded.")));
    }
    let block_list =
        List::new(block_items).block(Block::default().title(" Blocks ").borders(Borders::ALL));
    frame.render_widget(block_list, chunks[0]);

    let proc_count = scheduler.process_count();
    let ram_usage = scheduler.ram_usage();
    let running = scheduler.running_count();
    let mut proc_items = vec![
        ListItem::new(Line::from(format!("Total processes: {}", proc_count))),
        ListItem::new(Line::from(format!("Running: {}", running))),
        ListItem::new(Line::from(format!(
            "RAM used by processes: {} MB",
            ram_usage.0
        ))),
    ];
    for id in 0..5u64 {
        let pid = aios_process_mgr::task::ProcessId(id + 1);
        if let Some(proc) = scheduler.get_process(pid) {
            let state_str = match proc.state {
                aios_process_mgr::task::ProcessState::Running => "Running",
                aios_process_mgr::task::ProcessState::Ready => "Ready",
                aios_process_mgr::task::ProcessState::Suspended => "Suspended",
                aios_process_mgr::task::ProcessState::Terminated => "Terminated",
                aios_process_mgr::task::ProcessState::Crashed => "Crashed",
            };
            proc_items.push(ListItem::new(Line::from(format!(
                "  pid_{}: {} [{}]",
                proc.pid.0, proc.name, state_str
            ))));
        }
    }
    let proc_list =
        List::new(proc_items).block(Block::default().title(" Processes ").borders(Borders::ALL));
    frame.render_widget(proc_list, chunks[1]);
}

fn draw_ai_tab(frame: &mut Frame, area: Rect, app: &mut TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(4)])
        .split(area);

    let output: Vec<ListItem> = app
        .ai_output
        .lock()
        .unwrap()
        .iter()
        .map(|line| {
            if line.starts_with('>') {
                ListItem::new(Line::from(Span::styled(
                    line.clone(),
                    Style::default().fg(Color::Cyan),
                )))
            } else {
                ListItem::new(Line::from(line.clone()))
            }
        })
        .collect();
    let output_list =
        List::new(output).block(Block::default().title(" AI Console ").borders(Borders::ALL));
    frame.render_widget(output_list, chunks[0]);

    let input_style = if app.ai_mode {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let input_text = if app.ai_mode {
        format!(">> {}", app.ai_input)
    } else {
        " Press 'i' to enter query mode ".into()
    };
    let input_para = Paragraph::new(input_text)
        .style(input_style)
        .block(Block::default().title(" Input ").borders(Borders::ALL));
    frame.render_widget(input_para, chunks[1]);
}

fn draw_bridge_tab(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let state = app.state.lock().unwrap();
    let bridge_running = state
        .bridge_running
        .load(std::sync::atomic::Ordering::SeqCst);

    let url = format!("http://localhost:{}", app.bridge_port);
    let lines = vec![
        Line::from(vec![
            Span::raw("Bridge Server: "),
            Span::styled(
                if bridge_running {
                    "RUNNING"
                } else {
                    "STARTING..."
                },
                Style::default()
                    .fg(if bridge_running {
                        Color::Green
                    } else {
                        Color::Yellow
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("URL: "),
            Span::styled(
                &url,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]),
        Line::from(""),
        Line::from("Press 'g' to open in browser"),
        Line::from(""),
        Line::from("API Endpoints:"),
        Line::from("  GET  /api/v1/health          — System health check"),
        Line::from("  POST /api/v1/intent           — Submit user intent"),
        Line::from("  POST /api/v1/llm/query        — LLM query"),
        Line::from("  GET  /api/v1/system/status    — Full system status"),
        Line::from("  WS   /ws/telemetry            — Real-time telemetry stream"),
    ];
    let para = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .title(" Studio GUI Bridge ")
            .borders(Borders::ALL),
    );
    frame.render_widget(para, area);
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let count = app.displayed_logs.len();
    let start = count.saturating_sub(3);
    let recent: Vec<String> = app.displayed_logs.iter().skip(start).cloned().collect();

    let log_lines: Vec<ListItem> = recent
        .iter()
        .map(|l| {
            ListItem::new(Line::from(Span::styled(
                l.clone(),
                if l.contains("ERROR") {
                    Style::default().fg(Color::Red)
                } else if l.contains("WARN") {
                    Style::default().fg(Color::Yellow)
                } else if l.contains("Bridge") {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            )))
        })
        .collect();

    let title = if app.log_paused {
        " Events (PAUSED) "
    } else {
        " Events "
    };
    let list = List::new(log_lines)
        .block(Block::default().title(title).borders(Borders::ALL))
        .style(Style::default().fg(Color::DarkGray));

    let help = Line::from(vec![Span::raw(
        " [Tab/F1] tabs  [1-4] goto  [g] browser  [r] reprobe  [Space] pause  [q] quit ",
    )]);
    let help_style = Style::default().fg(Color::DarkGray).bg(Color::Black);

    let log_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    frame.render_widget(list, log_chunks[0]);
    frame.render_widget(Paragraph::new(help).style(help_style), log_chunks[1]);
}
