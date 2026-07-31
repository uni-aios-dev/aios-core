use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_browser::html_parser::HtmlParser;
use aios_context::persistence::PersistentStore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::{TelemetryEntry, TelemetryStore};
use aios_core::block::BlockId;
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use aios_tui::dashboard::{self, DashboardState, PageContent};
use aios_watchdog::heartbeat::Heartbeat;
use aios_watchdog::safe_mode::SafeModeShell;
use aios_watchdog::watchdog::{Watchdog, WatchdogConfig, WatchdogState};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn is_headless() -> bool {
    std::env::var("AIOS_HEADLESS").as_deref() == Ok("1")
        || std::env::args().any(|a| a == "--headless")
}

fn fetch_url(url: &str) -> Result<PageContent, Box<dyn std::error::Error>> {
    let resp = reqwest::blocking::get(url)?;
    let html = resp.text()?;
    let title = HtmlParser::extract_title(&html);
    let text = HtmlParser::extract_text(&html);
    let links = HtmlParser::extract_links(&html, url)
        .into_iter()
        .map(|l| (l.text, l.href))
        .collect();
    Ok(PageContent {
        url: url.to_string(),
        title,
        text,
        links,
    })
}

fn execute_shell_cmd(
    state: &mut DashboardState,
    cmd: &str,
    scheduler: &mut Scheduler,
    registry: &mut BlockRegistry,
    safe_shell: &mut SafeModeShell,
) {
    let lower = cmd.trim().to_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    let command = match parts.first().copied() {
        Some("clear") | Some("cls") => {
            state.shell_state.output.clear();
            return;
        }
        Some("fetch") => {
            let url = *parts.get(1).unwrap_or(&"");
            if url.is_empty() {
                state.shell_state.add_output("Usage: fetch <url>".into());
                return;
            }
            state
                .shell_state
                .add_output(format!("Fetching block from: {url}..."));
            match reqwest::blocking::get(url) {
                Ok(resp) => match resp.bytes() {
                    Ok(binary) => {
                        let name = url.split('/').next_back().unwrap_or("block");
                        match BlockLoader::load_from_binary(
                            registry,
                            name,
                            "1.0.0",
                            binary.to_vec(),
                        ) {
                            Ok(m) => state
                                .shell_state
                                .add_output(format!("Loaded block '{}' ID {}", m.name, m.id)),
                            Err(e) => state.shell_state.add_output(format!("Load failed: {e}")),
                        }
                    }
                    Err(e) => state.shell_state.add_output(format!("Read failed: {e}")),
                },
                Err(e) => state.shell_state.add_output(format!("Fetch failed: {e}")),
            }
            return;
        }
        Some("search") => {
            let query = parts.get(1..).map(|p| p.join(" ")).unwrap_or_default();
            if query.is_empty() {
                state.shell_state.add_output("Usage: search <query>".into());
                return;
            }
            state
                .shell_state
                .add_output(format!("Searching for: {query}..."));
            let url = format!(
                "https://html.duckduckgo.com/html/?q={}",
                urlencoding(&query)
            );
            match reqwest::blocking::get(&url) {
                Ok(resp) => {
                    if let Ok(html) = resp.text() {
                        let links = HtmlParser::extract_links(&html, &url);
                        state
                            .shell_state
                            .add_output(format!("Found {} results:", links.len()));
                        for (i, link) in links.iter().take(20).enumerate() {
                            let text = if link.text.is_empty() {
                                &link.href
                            } else {
                                &link.text
                            };
                            state.shell_state.add_output(format!(
                                "  {}. {} — {}",
                                i + 1,
                                text,
                                link.href
                            ));
                        }
                    }
                }
                Err(e) => state.shell_state.add_output(format!("Search failed: {e}")),
            }
            return;
        }
        Some("open") => {
            let url = (*parts.get(1).unwrap_or(&"")).to_string();
            if url.is_empty() {
                state.shell_state.add_output("Usage: open <url>".into());
                return;
            }
            state.selected_tab = 5;
            state.web_state.url_input = url.clone();
            state.web_state.loading = true;
            state.web_state.page = None;
            state.web_state.error = None;
            state.add_log(format!("Navigating to: {url}"));
            match fetch_url(&url) {
                Ok(page) => {
                    state.web_state.current_url = url;
                    state.web_state.page = Some(page);
                }
                Err(e) => state.web_state.error = Some(e.to_string()),
            }
            state.web_state.loading = false;
            return;
        }
        _ => aios_watchdog::safe_mode::ShellCommand::Unknown(cmd.to_string()),
    };
    let resp = safe_shell.execute(command, scheduler, registry);
    if resp.success {
        for line in resp.output.lines() {
            state.shell_state.add_output(line.to_string());
        }
    } else {
        state
            .shell_state
            .add_output(format!("Error: {}", resp.output));
    }
}

fn urlencoding(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn switch_tab(state: &mut DashboardState, tab: usize) {
    state.selected_tab = tab;
    state.selected_row = 0;
    state.process_kill_result = None;
    state.block_operation_result = None;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let data_dir = PathBuf::from(env_or("AIOS_DATA_DIR", "/app/data"));
    let blocks_dir = PathBuf::from(env_or("AIOS_BLOCKS_DIR", "/app/blocks"));
    let mock_profile = env_or("AIOS_MOCK_PROFILE", "modern");

    log::info!("AIOS: data_dir={:?}, blocks_dir={:?}", data_dir, blocks_dir);

    let _ = std::fs::create_dir_all(&data_dir);
    let _ = std::fs::create_dir_all(&blocks_dir);

    let profile = if mock_profile != "none" {
        log::info!("AIOS: using mock profile '{}'", mock_profile);
        match mock_profile.as_str() {
            "legacy" => HardwareProfile::mock_legacy(),
            _ => HardwareProfile::mock_modern(),
        }
    } else {
        HardwareProfile::detect()
    };

    let ai_tier = AiTier::from_profile(&profile);
    log::info!("AIOS: AI tier = {:?}", ai_tier);

    let mut registry = BlockRegistry::new();

    let hal_data = b"hal-native-module";
    let _ = BlockLoader::load_from_binary(&mut registry, "hal", "1.0.0", hal_data.to_vec());
    let _ = BlockLoader::load_from_binary(&mut registry, "ipc_bus", "1.0.0", b"ipc_bus".to_vec());
    let _ =
        BlockLoader::load_from_binary(&mut registry, "scheduler", "1.0.0", b"scheduler".to_vec());
    let _ = BlockLoader::load_from_binary(
        &mut registry,
        "browser",
        "0.1.0",
        b"browser-native".to_vec(),
    );

    let disk_results = BlockLoader::load_from_directory(&mut registry, &blocks_dir);
    let disk_loaded = disk_results.iter().filter(|r| r.is_ok()).count();
    let disk_failed = disk_results.iter().filter(|r| r.is_err()).count();
    log::info!(
        "AIOS: disk blocks loaded={}, failed={}",
        disk_loaded,
        disk_failed
    );

    registry.set_block_dependencies("ipc_bus", vec!["hal".into()]);
    registry.set_block_dependencies("scheduler", vec!["ipc_bus".into()]);

    let mut context_store = EmbeddedContextStore::new(10_000);
    if context_store.should_compact() {
        let report = context_store.compact();
        log::info!(
            "AIOS: auto-compact telemetry={}, workflows={}",
            report.telemetry_pruned,
            report.workflows_pruned
        );
    }

    let persistent = PersistentStore::new(data_dir.join("context.redb"));
    if persistent.is_available() {
        if let Some(version) = persistent.load_version() {
            log::info!("AIOS: recovered DB version={}", version);
        }
        if let Ok(telemetry) = persistent.load_telemetry() {
            log::info!("AIOS: recovered {} telemetry entries", telemetry.len());
            for entry in telemetry {
                context_store.telemetry_mut().record(entry);
            }
        }
    }

    let mut scheduler = Scheduler::new(profile.memory.total_mb);
    let _ = scheduler.spawn_process("ai_orchestrator", Priority::High, 512);
    let _ = scheduler.spawn_process("io_handler", Priority::Normal, 128);
    let _ = scheduler.spawn_process("health_monitor", Priority::Low, 64);

    let watchdog_config = WatchdogConfig {
        heartbeat_interval_ms: 2000,
        max_missed_heartbeats: 3,
        secret: b"aios_heartbeat_secret".to_vec(),
        ..Default::default()
    };
    let mut watchdog = Watchdog::new(watchdog_config.clone());
    watchdog
        .receive_heartbeat(&Heartbeat::new(0, &watchdog_config.secret))
        .ok();

    let watchdog_state = Arc::new(Mutex::new(watchdog.state()));
    let watchdog_state_clone = watchdog_state.clone();
    let hb_secret = watchdog_config.secret.clone();
    let hb_interval = watchdog_config.heartbeat_interval_ms;

    std::thread::spawn(move || {
        let mut seq: u64 = 1;
        loop {
            std::thread::sleep(Duration::from_millis(hb_interval / 2));
            let hb = Heartbeat::new(seq, &hb_secret);
            seq += 1;

            let state = if hb.verify(&hb_secret) {
                if seq.is_multiple_of(10) {
                    WatchdogState::Suspended
                } else {
                    WatchdogState::Monitoring
                }
            } else {
                WatchdogState::SafeMode
            };

            if let Ok(mut s) = watchdog_state_clone.lock() {
                *s = state;
            }
        }
    });

    let mut telemetry = TelemetryStore::new();
    let mut safe_shell = SafeModeShell::new(3);

    if is_headless() {
        log::info!("AIOS: headless mode — running without TUI");
        log::info!("AIOS: system initialized, entering background loop");
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = DashboardState::new(ai_tier, profile, &registry, &scheduler);

    log::info!("AIOS: TUI started — press q to quit");

    loop {
        let wd_state = watchdog_state
            .lock()
            .map(|s| *s)
            .unwrap_or(WatchdogState::Monitoring);
        state.update_watchdog(wd_state);

        terminal.draw(|f| {
            state.update_from_scheduler(&scheduler, &registry);
            dashboard::draw_dashboard(f, &state);
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if key.modifiers.contains(KeyModifiers::ALT) {
                        if let KeyCode::Char(d) = key.code {
                            if let Some(d) = d.to_digit(10) {
                                if (1..=7).contains(&d) {
                                    switch_tab(&mut state, (d - 1) as usize);
                                    continue;
                                }
                            }
                        }
                    }

                    if state.show_help {
                        match key.code {
                            KeyCode::F(1) | KeyCode::Char('?') | KeyCode::Esc => {
                                state.show_help = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if state.selected_tab == 5 && state.web_state.input_focused {
                        match key.code {
                            KeyCode::Esc => {
                                state.web_state.input_focused = false;
                            }
                            KeyCode::Enter => {
                                let url = state.web_state.url_input.clone();
                                if !url.is_empty() {
                                    state.web_state.loading = true;
                                    state.web_state.page = None;
                                    state.web_state.error = None;
                                    state.add_log(format!("Navigating to: {url}"));
                                    match fetch_url(&url) {
                                        Ok(page) => {
                                            state.web_state.current_url = url;
                                            state.web_state.page = Some(page);
                                            state.add_log("Loaded".to_string());
                                        }
                                        Err(e) => {
                                            state.web_state.error = Some(e.to_string());
                                            state.add_log("Fetch failed".to_string());
                                        }
                                    }
                                    state.web_state.loading = false;
                                }
                            }
                            KeyCode::Char(c) => {
                                state.web_state.url_input.push(c);
                            }
                            KeyCode::Backspace => {
                                state.web_state.url_input.pop();
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if state.block_input_mode != dashboard::BlockInputMode::None {
                        match key.code {
                            KeyCode::Esc => {
                                state.cancel_block_input();
                            }
                            KeyCode::Enter => {
                                let step = state.confirm_block_load();
                                if let Some((label, value)) = step {
                                    if label == "__name__" {
                                        state.add_log(format!("Load: entering name '{value}'"));
                                    } else if label == "__version__" {
                                        let path = std::path::PathBuf::from(format!(
                                            "{}/{}_{}.bin",
                                            std::env::var("AIOS_BLOCKS_DIR")
                                                .unwrap_or_else(|_| "/app/blocks".into()),
                                            state
                                                .blocks
                                                .get(state.selected_row)
                                                .map(|b| b.name.clone())
                                                .unwrap_or_default(),
                                            value
                                        ));
                                        if path.exists() {
                                            match std::fs::read(&path) {
                                                Ok(binary) => {
                                                    let name = state
                                                        .blocks
                                                        .get(state.selected_row)
                                                        .map(|b| b.name.clone())
                                                        .unwrap_or_else(|| "unknown".into());
                                                    match BlockLoader::load_from_binary(
                                                        &mut registry,
                                                        &name,
                                                        &value,
                                                        binary,
                                                    ) {
                                                        Ok(manifest) => {
                                                            state.add_log(format!(
                                                                "Loaded block '{}' ({})",
                                                                manifest.name, manifest.id
                                                            ));
                                                            state.block_operation_result =
                                                                Some(format!(
                                                                    "Loaded '{}' v{}",
                                                                    name, value
                                                                ));
                                                        }
                                                        Err(e) => {
                                                            state.add_log(format!(
                                                                "Load failed: {e}"
                                                            ));
                                                            state.block_operation_result =
                                                                Some(format!("Load failed: {e}"));
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    state.add_log(format!(
                                                        "Failed to read {}: {e}",
                                                        path.display()
                                                    ));
                                                    state.block_operation_result =
                                                        Some(format!("Read failed: {e}"));
                                                }
                                            }
                                        } else {
                                            state.add_log(format!(
                                                "Block file not found: {}",
                                                path.display()
                                            ));
                                            state.block_operation_result =
                                                Some(format!("File not found: {}", path.display()));
                                        }
                                    }
                                }
                            }
                            KeyCode::Char(c) => {
                                state.push_char_to_block_input(c);
                            }
                            KeyCode::Backspace => {
                                state.pop_char_from_block_input();
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if state.selected_tab == 6 {
                        match key.code {
                            KeyCode::Enter => {
                                let cmd = state.shell_state.input_buffer.trim().to_string();
                                if !cmd.is_empty() {
                                    state.shell_state.add_output(format!("$ {cmd}"));
                                    state.shell_state.push_history(cmd.clone());
                                    execute_shell_cmd(
                                        &mut state,
                                        &cmd,
                                        &mut scheduler,
                                        &mut registry,
                                        &mut safe_shell,
                                    );
                                    state.shell_state.input_buffer.clear();
                                }
                            }
                            KeyCode::Backspace => {
                                state.shell_state.input_buffer.pop();
                            }
                            KeyCode::Up => {
                                if state.shell_state.history_pos > 0 {
                                    state.shell_state.history_pos -= 1;
                                    state.shell_state.input_buffer = state
                                        .shell_state
                                        .command_history[state.shell_state.history_pos]
                                        .clone();
                                }
                            }
                            KeyCode::Down => {
                                let len = state.shell_state.command_history.len();
                                if state.shell_state.history_pos < len {
                                    state.shell_state.history_pos += 1;
                                    state.shell_state.input_buffer =
                                        if state.shell_state.history_pos < len {
                                            state.shell_state.command_history
                                                [state.shell_state.history_pos]
                                                .clone()
                                        } else {
                                            String::new()
                                        };
                                }
                            }
                            KeyCode::Esc => {
                                state.shell_state.input_buffer.clear();
                            }
                            KeyCode::Char(c) => {
                                state.shell_state.input_buffer.push(c);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::F(1) | KeyCode::Char('?') => {
                            state.show_help = true;
                        }
                        KeyCode::Char('q') => break,
                        KeyCode::Esc => {
                            if state.show_help {
                                state.show_help = false;
                            } else {
                                break;
                            }
                        }
                        KeyCode::Char('1') => switch_tab(&mut state, 0),
                        KeyCode::Char('2') => switch_tab(&mut state, 1),
                        KeyCode::Char('3') => switch_tab(&mut state, 2),
                        KeyCode::Char('4') => switch_tab(&mut state, 3),
                        KeyCode::Char('5') => switch_tab(&mut state, 4),
                        KeyCode::Char('6') => switch_tab(&mut state, 5),
                        KeyCode::Char('7') => switch_tab(&mut state, 6),
                        KeyCode::Char('j') | KeyCode::Down => {
                            state.move_selection_down();
                            state.process_kill_result = None;
                            state.block_operation_result = None;
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            state.move_selection_up();
                            state.process_kill_result = None;
                            state.block_operation_result = None;
                        }
                        KeyCode::Char('K') => {
                            if state.selected_tab == 1 {
                                if let Some(pid) = state.selected_process_pid() {
                                    use aios_process_mgr::task::ProcessId;
                                    let result = scheduler.kill_process(ProcessId(pid));
                                    match result {
                                        Ok(proc) => {
                                            state.add_log(format!(
                                                "Killed process '{}' (PID {})",
                                                proc.name, pid
                                            ));
                                            state.process_kill_result = Some(format!(
                                                "Killed '{}' (PID {})",
                                                proc.name, pid
                                            ));
                                        }
                                        Err(e) => {
                                            state.add_log(format!("Kill failed: {e}"));
                                            state.process_kill_result =
                                                Some(format!("Kill failed: {e}"));
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('U') => {
                            if state.selected_tab == 2 {
                                if let Some((name, version)) = state.selected_block_name_version() {
                                    let selected_id =
                                        state.blocks.get(state.selected_row).map(|b| BlockId(b.id));
                                    if let Some(id) = selected_id {
                                        match registry.unload_block(id) {
                                            Ok(entry) => {
                                                state.add_log(format!(
                                                    "Unloaded block '{}' ({})",
                                                    entry.manifest.name, id
                                                ));
                                                state.block_operation_result =
                                                    Some(format!("Unloaded '{name}@{version}'"));
                                            }
                                            Err(e) => {
                                                state.add_log(format!("Unload failed: {e}"));
                                                state.block_operation_result =
                                                    Some(format!("Unload failed: {e}"));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('L') => {
                            if state.selected_tab == 2 {
                                state.start_load_block();
                                state.add_log("Loading block: enter name...".into());
                            }
                        }
                        KeyCode::Char('H') => {
                            if state.selected_tab == 2 {
                                if let Some((name, version)) = state.selected_block_name_version() {
                                    let selected_id =
                                        state.blocks.get(state.selected_row).map(|b| BlockId(b.id));
                                    let path = std::path::PathBuf::from(format!(
                                        "{}/{}_{}.bin",
                                        std::env::var("AIOS_BLOCKS_DIR")
                                            .unwrap_or_else(|_| "/app/blocks".into()),
                                        name,
                                        version
                                    ));
                                    if path.exists() {
                                        if let Some(id) = selected_id {
                                            let _ = registry.unload_block(id);
                                        }
                                        match std::fs::read(&path) {
                                            Ok(binary) => {
                                                match BlockLoader::load_from_binary(
                                                    &mut registry,
                                                    &name,
                                                    &version,
                                                    binary,
                                                ) {
                                                    Ok(manifest) => {
                                                        state.add_log(format!(
                                                            "Hot-swap: reloaded '{}' ({})",
                                                            manifest.name, manifest.id
                                                        ));
                                                        state.block_operation_result =
                                                            Some(format!(
                                                                "Hot-swap OK: '{name}@{version}'"
                                                            ));
                                                    }
                                                    Err(e) => {
                                                        state.block_operation_result =
                                                            Some(format!("Hot-swap failed: {e}"));
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                state
                                                    .add_log(format!("Hot-swap: read failed: {e}"));
                                                state.block_operation_result =
                                                    Some(format!("Hot-swap read failed: {e}"));
                                            }
                                        }
                                    } else {
                                        state.add_log(format!(
                                            "Hot-swap: binary not found at {}",
                                            path.display()
                                        ));
                                        state.block_operation_result =
                                            Some(format!("Binary not found: {}", path.display()));
                                    }
                                }
                            }
                        }
                        KeyCode::Char('g') => {
                            if state.selected_tab == 5 {
                                state.web_state.input_focused = true;
                                state.add_log("URL bar focused — type URL, Enter to go".into());
                            }
                        }
                        KeyCode::Char('o') => {
                            if state.selected_tab == 5 {
                                if let Some(ref page) = state.web_state.page {
                                    if let Some((_text, href)) = page.links.get(state.selected_row)
                                    {
                                        let href = href.clone();
                                        state.web_state.url_input = href.clone();
                                        state.web_state.loading = true;
                                        state.web_state.page = None;
                                        state.web_state.error = None;
                                        state.add_log(format!("Opening: {href}"));
                                        match fetch_url(&href) {
                                            Ok(p) => {
                                                state.web_state.current_url = href;
                                                state.web_state.page = Some(p);
                                                state.add_log("Loaded".to_string());
                                            }
                                            Err(e) => {
                                                state.web_state.error = Some(e.to_string());
                                                state.add_log("Fetch failed".to_string());
                                            }
                                        }
                                        state.web_state.loading = false;
                                    }
                                }
                            }
                        }
                        KeyCode::Char('r') => {
                            state.add_log("System refreshed".into());
                        }
                        KeyCode::Char('s') => {
                            let entry = TelemetryEntry::new(
                                "process_count",
                                scheduler.process_count() as f64,
                                scheduler.ram_usage().0,
                            )
                            .with_process("scheduler");
                            telemetry.record(entry);
                            let avg = telemetry.average_value("process_count").unwrap_or(0.0);
                            context_store.telemetry_mut().record(
                                TelemetryEntry::new("process_count", avg, 0)
                                    .with_process("scheduler"),
                            );
                            state.add_log(format!(
                                "Telemetry: {} proc, avg={:.1}",
                                scheduler.process_count(),
                                avg,
                            ));
                        }
                        KeyCode::Char('x') => {
                            let resp = safe_shell.execute(
                                aios_watchdog::safe_mode::ShellCommand::SystemStatus,
                                &mut scheduler,
                                &mut registry,
                            );
                            state.add_log(format!("Status: {}", resp.output));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    log::info!("AIOS: saving state before shutdown...");

    let telemetry_entries: Vec<TelemetryEntry> = context_store.telemetry().entries.to_vec();
    if !telemetry_entries.is_empty() {
        match persistent.save_telemetry(&telemetry_entries) {
            Ok(n) => log::info!("AIOS: persisted {} telemetry entries", n),
            Err(e) => log::error!("AIOS: failed to persist telemetry: {}", e),
        }
    }
    match persistent.save_version("1.0.0") {
        Ok(_) => log::info!("AIOS: saved DB version"),
        Err(e) => log::error!("AIOS: failed to save version: {}", e),
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    log::info!("AIOS: shutdown complete");
    Ok(())
}
