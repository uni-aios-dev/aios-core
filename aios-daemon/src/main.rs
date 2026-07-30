use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_context::persistence::PersistentStore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::TelemetryEntry;
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use aios_watchdog::heartbeat::Heartbeat;
use aios_watchdog::watchdog::{Watchdog, WatchdogConfig, WatchdogState};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let data_dir = PathBuf::from(env_or("AIOS_DATA_DIR", "/app/data"));
    let blocks_dir = PathBuf::from(env_or("AIOS_BLOCKS_DIR", "/app/blocks"));
    let mock_profile = env_or("AIOS_MOCK_PROFILE", "modern");

    log::info!(
        "AIOS daemon: data_dir={:?}, blocks_dir={:?}",
        data_dir,
        blocks_dir
    );

    let _ = std::fs::create_dir_all(&data_dir);
    let _ = std::fs::create_dir_all(&blocks_dir);

    let profile = if mock_profile != "none" {
        log::info!("AIOS daemon: using mock profile '{}'", mock_profile);
        match mock_profile.as_str() {
            "legacy" => HardwareProfile::mock_legacy(),
            _ => HardwareProfile::mock_modern(),
        }
    } else {
        HardwareProfile::detect()
    };

    let ai_tier = AiTier::from_profile(&profile);
    log::info!("AIOS daemon: AI tier = {:?}", ai_tier);

    let mut registry = BlockRegistry::new();

    let _ =
        BlockLoader::load_from_binary(&mut registry, "hal", "1.0.0", b"hal-native-module".to_vec());
    let _ = BlockLoader::load_from_binary(&mut registry, "ipc_bus", "1.0.0", b"ipc_bus".to_vec());
    let _ =
        BlockLoader::load_from_binary(&mut registry, "scheduler", "1.0.0", b"scheduler".to_vec());

    let disk_results = BlockLoader::load_from_directory(&mut registry, &blocks_dir);
    let disk_loaded = disk_results.iter().filter(|r| r.is_ok()).count();
    let disk_failed = disk_results.iter().filter(|r| r.is_err()).count();
    log::info!(
        "AIOS daemon: disk blocks loaded={}, failed={}",
        disk_loaded,
        disk_failed
    );

    registry.set_block_dependencies("ipc_bus", vec!["hal".into()]);
    registry.set_block_dependencies("scheduler", vec!["ipc_bus".into()]);

    let mut context_store = EmbeddedContextStore::new(10_000);
    if context_store.should_compact() {
        let report = context_store.compact();
        log::info!(
            "AIOS daemon: auto-compact telemetry={}, workflows={}",
            report.telemetry_pruned,
            report.workflows_pruned
        );
    }

    let persistent = PersistentStore::new(data_dir.join("context.redb"));
    if persistent.is_available() {
        if let Some(version) = persistent.load_version() {
            log::info!("AIOS daemon: recovered DB version={}", version);
        }
        if let Ok(telemetry) = persistent.load_telemetry() {
            log::info!(
                "AIOS daemon: recovered {} telemetry entries",
                telemetry.len()
            );
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

    log::info!("AIOS daemon: initialized successfully");

    loop {
        std::thread::sleep(Duration::from_secs(10));

        let wd_state = watchdog_state
            .lock()
            .map(|s| *s)
            .unwrap_or(WatchdogState::Monitoring);

        let proc_count = scheduler.process_count();
        let ram = scheduler.ram_usage();

        log::info!(
            "AIOS daemon: heartbeat — processes={}, ram={}MB, watchdog={:?}",
            proc_count,
            ram.0,
            wd_state,
        );

        let entry = TelemetryEntry::new("process_count", proc_count as f64, ram.0);
        context_store.telemetry_mut().record(entry);

        if seq_num() % 6 == 0 {
            let telemetry_entries: Vec<TelemetryEntry> = context_store.telemetry().entries.to_vec();
            if !telemetry_entries.is_empty() {
                let _ = persistent.save_telemetry(&telemetry_entries);
            }
            let _ = persistent.save_version("1.0.0");
        }
    }
}

fn seq_num() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}
