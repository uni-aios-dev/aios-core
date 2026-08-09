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
    " Studio Bridge ",
    " Network & Store ",
    " Web ",
    " Shell ",
];

pub fn draw(frame: &mut Frame, app: &mut TuiApp) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(10),
            Constraint::Length(7),
        ])
        .split(area);

    draw_header(frame, chunks[0], app);
    draw_tabs(frame, chunks[1], app);
    match app.current_tab {
        0 => draw_system_tab(frame, chunks[2], app),
        1 => draw_blocks_tab(frame, chunks[2], app),
        2 => draw_ai_tab(frame, chunks[2], app),
        3 => draw_bridge_tab(frame, chunks[2], app),
        4 => draw_net_store_tab(frame, chunks[2], app),
        5 => draw_web_tab(frame, chunks[2], app),
        6 => draw_shell_tab(frame, chunks[2], app),
        _ => {}
    }
    draw_logs(frame, chunks[3], app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let state = app.state.lock().unwrap();
    let s = state.start_time.elapsed().as_secs();
    let uptime = format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60);

    let header = Line::from(vec![
        Span::styled(
            " AIOS v2.9.1 ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ Status: "),
        Span::styled(
            if state.safe_mode { "SAFE MODE" } else { "OK" },
            Style::default()
                .fg(if state.safe_mode {
                    Color::Yellow
                } else {
                    Color::Green
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ Uptime: "),
        Span::styled(uptime, Style::default().fg(Color::Yellow)),
        Span::raw(" │ AI Tier: "),
        Span::styled(
            state.hw_profile.ai_tier.clone(),
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

    let mut os_lines = vec![
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
    if let Some(ref gpu) = hw.gpu {
        let gpu_name = if gpu.vram_gb > 0.0 {
            format!("GPU: {} ({:.1} GB VRAM)", gpu.model, gpu.vram_gb)
        } else {
            format!("GPU: {}", gpu.model)
        };
        os_lines.push(Line::from(gpu_name));
    }
    os_lines.push(Line::from(format!("AI Tier: {}", hw.ai_tier)));
    if state.safe_mode {
        os_lines.push(Line::from(format!(
            "Boot: {}",
            "SAFE MODE — third-party blocks and bridge disabled"
        )));
    }

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
    // Lock order must match the bridge (scheduler → registry) to avoid a
    // deadlock between the TUI render thread and bridge request handlers.
    let scheduler = state.bridge.scheduler.lock().unwrap();
    let registry = state.bridge.registry.lock().unwrap();

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let mut ids = registry.all_ids();
    ids.sort_by_key(|id| id.0);
    let mut block_items: Vec<ListItem> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let base = match registry.get(*id) {
                Ok(entry) => {
                    let status = match entry.state {
                        aios_core::block::BlockState::Active => "Active",
                        aios_core::block::BlockState::Loaded => "Loaded",
                        aios_core::block::BlockState::Error => "Error",
                        _ => "Other",
                    };
                    format!(
                        "[{}] {} v{} [{}]",
                        id, entry.manifest.name, entry.manifest.version, status
                    )
                }
                Err(_) => format!("[{}] error", id),
            };
            let style = if i == app.blocks_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(base, style)))
        })
        .collect();
    if block_items.is_empty() {
        block_items.push(ListItem::new(Line::from("No blocks loaded.")));
    }
    let block_list = List::new(block_items).block(
        Block::default()
            .title(" Blocks (j/k select, r restart, k unload, l load) ")
            .borders(Borders::ALL),
    );
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
    for proc in scheduler.all_processes() {
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
    let proc_list =
        List::new(proc_items).block(Block::default().title(" Processes ").borders(Borders::ALL));
    frame.render_widget(proc_list, chunks[1]);
}

fn draw_ai_tab(frame: &mut Frame, area: Rect, app: &mut TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    if app.ai_show_help {
        draw_ai_help(frame, chunks[0]);
    } else {
        draw_ai_output(frame, chunks[0], app);
    }

    let input_style = if app.ai_mode {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let input_text = if app.ai_mode {
        format!(">> {}", app.ai_input)
    } else {
        " Press 'i' to enter query mode  |  'h' for help ".into()
    };
    let input_para = Paragraph::new(input_text)
        .style(input_style)
        .block(Block::default().title(" Input ").borders(Borders::ALL));
    frame.render_widget(input_para, chunks[1]);

    let status = app.ai_status.lock().unwrap().clone();
    let cfg = &app.ai_config;
    let backend = match cfg.backend {
        aios_llm::BackendKind::Cloud(ref p) => format!("cloud/{}", aios_llm::provider_name(p)),
        aios_llm::BackendKind::MicroLocal => "local/micro".into(),
        aios_llm::BackendKind::FullLocal => "local/full".into(),
    };
    let status_line = format!(
        " {backend} | {} | temp {} | tokens {} | {status} ",
        cfg.model, cfg.temperature, cfg.max_tokens
    );
    let status_para = Paragraph::new(status_line).style(Style::default().fg(Color::Cyan));
    frame.render_widget(status_para, chunks[2]);
}

fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if cur.chars().count() >= width {
            out.push(cur);
            cur = String::new();
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn draw_ai_output(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let width = area.width.saturating_sub(2).max(1) as usize;
    let mut items: Vec<ListItem> = Vec::new();
    {
        let guard = app.ai_output.lock().unwrap();
        for line in guard.iter() {
            let (style, text) = if line.starts_with('>') {
                (Style::default().fg(Color::Cyan), format!("  {line}"))
            } else if line.starts_with("[error]") {
                (Style::default().fg(Color::Red), line.clone())
            } else {
                (Style::default().fg(Color::White), line.clone())
            };
            for wrapped in wrap_line(&text, width) {
                items.push(ListItem::new(Line::from(Span::styled(wrapped, style))));
            }
        }
    }
    if *app.ai_streaming.lock().unwrap() {
        let partial = app.ai_stream.lock().unwrap().clone();
        if !partial.is_empty() {
            for wrapped in wrap_line(&partial, width) {
                items.push(ListItem::new(Line::from(Span::styled(
                    wrapped,
                    Style::default().fg(Color::Yellow),
                ))));
            }
        } else {
            items.push(ListItem::new(Line::from(Span::styled(
                " …",
                Style::default().fg(Color::Yellow),
            ))));
        }
    }
    if items.is_empty() {
        items.push(ListItem::new(Line::from(
            " Type a message and press Enter, or type /help for the command reference. ",
        )));
    }
    let output_list =
        List::new(items).block(Block::default().title(" AI Console ").borders(Borders::ALL));
    frame.render_widget(output_list, area);
}

const AI_HELP: &[&str] = &[
    "=== AI Console Help ===",
    "",
    "Keys:",
    "  i            enter query mode",
    "  Enter        send query or run command",
    "  Up / Down    navigate prompt history",
    "  Esc          exit query mode / close help",
    "  h            toggle this help panel",
    "  q            quit AIOS (when not typing)",
    "",
    "Slash commands (type '/<command>' then Enter):",
    "  /help            open this panel",
    "  /status          show backend, model and parameter info",
    "  /clear           clear the chat output",
    "  /history         show the last prompts",
    "  /system <text>   set the system prompt",
    "  /model <name>    set the model (e.g. llama-3.3-70b-versatile)",
    "  /backend <kind>  groq | openrouter | google | micro | full",
    "  /key <api-key>   set the API key (no argument clears it)",
    "  /temp <0.0-2.0>  set sampling temperature",
    "  /tokens <1-8192> set max output tokens",
    "  /preset <name>    apply a prompt template",
    "  /preset <name> <text>  save a prompt template",
    "  /preset list      list templates | /preset del <name> delete",
    "  /save            persist the chat to disk",
    "  /load            restore the chat from disk",
    "",
    "Notes:",
    "  * Cloud backends need an API key (AIOS_LLM_API_KEY or /key).",
    "  * Local backends need a GGUF model (AIOS_MODEL_PATH / AIOS_MODELS_DIR).",
    "  * Changes are applied to the shared engine, so the HTTP",
    "    /api/v1/llm/query endpoint uses the same configuration.",
    "  * Responses stream in live; the chat is auto-saved to disk after",
    "    each reply and restored on the next boot (AIOS_DATA_DIR/chat.jsonl).",
];

fn draw_ai_help(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = AI_HELP
        .iter()
        .map(|l| {
            if l.starts_with("===") {
                Line::from(Span::styled(
                    *l,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if l.starts_with("  /") {
                Line::from(Span::styled(*l, Style::default().fg(Color::Cyan)))
            } else {
                Line::from(*l)
            }
        })
        .collect();
    let para = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title(" AI Console Help ")
                .borders(Borders::ALL),
        )
        .style(Style::default().fg(Color::White));
    frame.render_widget(para, area);
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

fn draw_net_store_tab(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);

    let mut net_lines = vec![
        Line::from("Network Settings (via aios-net-config block / IPC):"),
        Line::from("  hostname | listen_port | dhcp_enabled | dns_server | gateway"),
    ];
    if !app.net_status.is_empty() {
        net_lines.push(Line::from(vec![
            Span::raw("  Current: "),
            Span::styled(app.net_status.clone(), Style::default().fg(Color::Cyan)),
        ]));
    }
    if app.net_mode {
        net_lines.push(Line::from(Span::styled(
            format!("  set> {}", app.net_input),
            Style::default().fg(Color::Green),
        )));
    }
    let net_para = Paragraph::new(Text::from(net_lines))
        .block(Block::default().title(" Network ").borders(Borders::ALL));
    frame.render_widget(net_para, chunks[0]);

    let mut store_items: Vec<ListItem> = app
        .store_installed
        .iter()
        .map(|b| ListItem::new(Line::from(format!(" • {b}"))))
        .collect();
    if store_items.is_empty() {
        store_items.push(ListItem::new(Line::from(
            " No installed blocks. Press 's' to refresh the block store. ",
        )));
    }
    let store_list = List::new(store_items).block(
        Block::default()
            .title(" Block Store ")
            .borders(Borders::ALL),
    );
    frame.render_widget(store_list, chunks[1]);

    let status = if app.store_status.is_empty() {
        " 'n' edit network  'g' show current net config  's' refresh store  (see Shell: store list/search/install) ".to_string()
    } else {
        format!(
            " {}  |  'n' edit net  'g' show net config  's' refresh store ",
            app.store_status
        )
    };
    let status_para = Paragraph::new(status).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(status_para, chunks[2]);
}

fn draw_web_tab(frame: &mut Frame, area: Rect, app: &mut TuiApp) {
    let sidebar_width = 24.min(area.width / 3).max(10);
    let content_width = area.width.saturating_sub(sidebar_width);
    app.web.wrap_width = content_width.saturating_sub(3) as usize;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(sidebar_width)])
        .split(area);

    let mut content_items: Vec<ListItem> = Vec::new();
    if app.web.tabs.len() > 1 {
        let mut spans = Vec::new();
        spans.push(Span::styled(
            " Tabs: ",
            Style::default().fg(Color::DarkGray),
        ));
        for (i, tab) in app.web.tabs.iter().enumerate() {
            let label = if tab.url.is_empty() {
                "new".to_string()
            } else {
                compact_label(&tab.url, 16)
            };
            let txt = format!("[{}] {} ", i + 1, label);
            let style = if i == app.web.active_tab {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(txt, style));
        }
        spans.push(Span::styled(
            "t=new x=close [ ]=switch ",
            Style::default().fg(Color::DarkGray),
        ));
        content_items.push(ListItem::new(Line::from(spans)));
    }
    if app.web.bookmark_naming {
        content_items.push(ListItem::new(Line::from(Span::styled(
            format!("Bookmark name: {}", app.web.bookmark_name),
            Style::default().fg(Color::Green),
        ))));
    }
    let url_line = if app.web.input_focused {
        format!("URL/query: {}", app.web.url_input)
    } else if !app.web.current_url.is_empty() {
        app.web.current_url.clone()
    } else {
        " No page loaded — press 'g' to enter a URL or search query ".into()
    };
    content_items.push(ListItem::new(Line::from(Span::styled(
        url_line,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))));

    if app.web.loading {
        content_items.push(ListItem::new(Line::from(Span::styled(
            " Loading... ",
            Style::default().fg(Color::Yellow),
        ))));
    } else if let Some(ref err) = app.web.error {
        content_items.push(ListItem::new(Line::from(Span::styled(
            format!(" Error: {err} "),
            Style::default().fg(Color::Red),
        ))));
    } else if let Some(ref page) = app.web.page {
        let lines = super::wrap_text(&page.text_content, app.web.wrap_width);
        for line in lines.iter().skip(app.web.scroll) {
            content_items.push(ListItem::new(Line::from(line.clone())));
        }
    } else {
        content_items.push(ListItem::new(Line::from(
            " Text-mode browser. Type 'g' to navigate, j/k to move between links, 'o' to open. ",
        )));
    }
    if !app.web.bookmark_naming && !app.web.current_url.is_empty() && app.web.page.is_some() {
        content_items.push(ListItem::new(Line::from(Span::styled(
            format!(
                " 'a' bookmark  'm' bookmarks ({}) ",
                app.web.bookmarks.len()
            ),
            Style::default().fg(Color::DarkGray),
        ))));
    }

    let content = List::new(content_items).block(
        Block::default()
            .title(if app.web.bookmark_naming {
                " Text Browser — New Bookmark ".to_string()
            } else if app.web.input_focused {
                " Text Browser — Enter URL ".to_string()
            } else if app.web.tabs.len() > 1 {
                format!(
                    " Text Browser — Tab {}/{} ",
                    app.web.active_tab + 1,
                    app.web.tabs.len()
                )
            } else {
                " Text Browser ".to_string()
            })
            .borders(Borders::ALL),
    );
    frame.render_widget(content, chunks[0]);

    let right_items: Vec<ListItem> = if app.web.show_bookmarks {
        if app.web.bookmarks.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                " No bookmarks yet — press 'a' to add ",
                Style::default().fg(Color::DarkGray),
            )))]
        } else {
            app.web
                .bookmarks
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let label: String = if b.name.is_empty() {
                        compact_label(&b.url, sidebar_width.saturating_sub(4) as usize)
                    } else {
                        compact_label(&b.name, sidebar_width.saturating_sub(4) as usize)
                    };
                    let style = if i == app.web.bookmarks_sel {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                    } else {
                        Style::default().fg(Color::Cyan)
                    };
                    ListItem::new(Line::from(Span::styled(label, style)))
                })
                .collect()
        }
    } else {
        match app.web.page {
            Some(ref page) => page
                .links
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let style = if i == app.web.selected_link {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                    } else {
                        Style::default().fg(Color::Cyan)
                    };
                    ListItem::new(Line::from(Span::styled(l.text.clone(), style)))
                })
                .collect(),
            None => vec![ListItem::new(Line::from(" No links yet "))],
        }
    };
    let right_title = if app.web.show_bookmarks {
        " Bookmarks — j/k o d Esc "
    } else {
        " Links "
    };
    let links =
        List::new(right_items).block(Block::default().title(right_title).borders(Borders::ALL));
    frame.render_widget(links, chunks[1]);
}

/// Truncate `s` to at most `width` characters (by chars, not bytes).
fn compact_label(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    if s.chars().count() > width {
        out.push('…');
    }
    out
}

fn draw_shell_tab(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    let height = chunks[0].height.saturating_sub(2) as usize;
    let start = app.shell_output.len().saturating_sub(height);
    let mut items: Vec<ListItem> = app
        .shell_output
        .iter()
        .skip(start)
        .map(|l| {
            let style = if l.starts_with("$ ") {
                Style::default().fg(Color::Cyan)
            } else if l.starts_with("AIOS:") {
                Style::default().fg(Color::DarkGray)
            } else if l.contains("ERROR") {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(l.clone(), style)))
        })
        .collect();
    if items.is_empty() {
        items.push(ListItem::new(Line::from(
            " Shell ready — type 'help' for the command reference. ",
        )));
    }
    let out_list = List::new(items).block(Block::default().title(" Shell ").borders(Borders::ALL));
    frame.render_widget(out_list, chunks[0]);

    let input = Paragraph::new(format!("$ {}", app.shell_input))
        .style(Style::default().fg(Color::Green))
        .block(Block::default().title(" Input ").borders(Borders::ALL));
    frame.render_widget(input, chunks[1]);
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

    let net_line = if app.net_mode {
        Line::from(vec![
            Span::styled(
                " net: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(app.net_input.clone(), Style::default().fg(Color::Green)),
            Span::raw("  [Enter] apply  [Esc] cancel  (e.g. hostname=server-1 listen_port=8080)"),
        ])
    } else {
        Line::from(Span::raw(
            " [n] Change network settings via IPC (hostname, listen_port, dhcp_enabled, dns_server) ",
        ))
    };
    let net_style = if app.net_mode {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray).bg(Color::Black)
    };

    let help = Line::from(vec![Span::raw(
        " [Tab/F1] tabs  [1-7] goto  [W] GUI  [Space] pause  [q] quit  | Web: g nav j/k links o open u/d scroll b back t tab x close [ ] switch a bkmk m list B native ",
    )]);

    let log_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    frame.render_widget(list, log_chunks[0]);
    frame.render_widget(Paragraph::new(net_line).style(net_style), log_chunks[1]);
    frame.render_widget(help, log_chunks[2]);
}
