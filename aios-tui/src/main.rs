use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
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
use aios_context::persistence::PersistentStore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::{TelemetryEntry, TelemetryStore};
use aios_core::block::BlockId;
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use aios_tui::dashboard::{self, DashboardState};
use aios_watchdog::heartbeat::Heartbeat;
use aios_watchdog::safe_mode::SafeModeShell;
use aios_watchdog::watchdog::{Watchdog, WatchdogConfig, WatchdogState};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
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
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('1') => {
                            state.selected_tab = 0;
                            state.selected_row = 0;
                            state.process_kill_result = None;
                        }
                        KeyCode::Char('2') => {
                            state.selected_tab = 1;
                            state.selected_row = 0;
                            state.process_kill_result = None;
                        }
                        KeyCode::Char('3') => {
                            state.selected_tab = 2;
                            state.selected_row = 0;
                            state.process_kill_result = None;
                        }
                        KeyCode::Char('4') => {
                            state.selected_tab = 3;
                            state.selected_row = 0;
                            state.process_kill_result = None;
                        }
                        KeyCode::Char('5') => {
                            state.selected_tab = 4;
                            state.selected_row = 0;
                            state.process_kill_result = None;
                            state.block_operation_result = None;
                        }
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
