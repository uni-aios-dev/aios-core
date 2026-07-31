use crate::hw_probe::{self, HwProfile};
use aios_block_mgr::registry::BlockRegistry;
use aios_block_mgr::router::MessageRouter;
use aios_bridge::server::{start_server, BridgeContext};
use aios_browser::block::BrowserBlock;
use aios_browser::types::BrowserConfig;
use aios_core::block::BlockId;
use aios_core::block::StatefulBlock;
use aios_llm::{default_config, LlmEngine};
use aios_process_mgr::scheduler::Scheduler;
use aios_security::access_control::AccessControlLayer;
use aios_wasm::executor::BlockExecutor;
use aios_watchdog::watchdog::{Watchdog, WatchdogConfig};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct OrchestratorState {
    pub hw_profile: HwProfile,
    pub bridge: Arc<BridgeContext>,
    pub router: MessageRouter,
    pub browser_block_id: BlockId,
    pub start_time: Instant,
    pub bridge_running: Arc<AtomicBool>,
    pub logs: Arc<Mutex<Vec<String>>>,
}

pub struct AppConfig {
    pub bridge_port: u16,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { bridge_port: 8080 }
    }
}

pub fn push_log(logs: &Arc<Mutex<Vec<String>>>, msg: String) {
    if let Ok(mut guard) = logs.lock() {
        guard.push(msg);
        if guard.len() > 1000 {
            let len = guard.len();
            guard.drain(0..len - 500);
        }
    }
}

pub async fn initialize(
    config: &AppConfig,
) -> Result<OrchestratorState, Box<dyn std::error::Error>> {
    let logs = Arc::new(Mutex::new(Vec::new()));
    push_log(&logs, "AIOS: probing hardware...".into());

    let hw_profile = hw_probe::probe();
    push_log(
        &logs,
        format!("AIOS: detected CPU: {}", hw_profile.cpu.brand),
    );
    push_log(
        &logs,
        format!("AIOS: RAM: {:.1} GB total", hw_profile.memory.total_gb),
    );
    if let Some(ref gpu) = hw_profile.gpu {
        push_log(
            &logs,
            format!("AIOS: GPU: {} ({:.1} GB VRAM)", gpu.model, gpu.vram_gb),
        );
    }
    push_log(&logs, format!("AIOS: AI tier: {}", hw_profile.ai_tier));

    push_log(&logs, "AIOS: initializing scheduler...".into());
    let total_ram_mb = hw_profile.memory.total_bytes / 1_048_576;
    let scheduler = Scheduler::new(total_ram_mb)
        .with_aging_threshold(5000)
        .with_time_slice(100)
        .with_max_restarts(5);

    push_log(&logs, "AIOS: initializing block registry...".into());
    let mut registry = BlockRegistry::new();

    for (name, version, binary) in [
        ("hal", "1.0.0", &b"hal-native-module"[..]),
        ("ipc_bus", "1.0.0", &b"ipc_bus"[..]),
        ("scheduler", "1.0.0", &b"scheduler"[..]),
        ("browser", "0.1.0", &b"browser-native"[..]),
    ] {
        if let Ok(id) = registry.register_block(name, version, binary.to_vec()) {
            let _ = registry.activate_block(id);
        }
    }
    registry.set_block_dependencies("ipc_bus", vec!["hal".into()]);
    registry.set_block_dependencies("scheduler", vec!["ipc_bus".into()]);

    let blocks_dir = std::env::var("AIOS_BLOCKS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("blocks"));
    let disk_results = registry.boot_discover(&blocks_dir);
    push_log(
        &logs,
        format!(
            "AIOS: registered {} core blocks, discovered {} disk blocks",
            registry.count() - disk_results.len(),
            disk_results.len()
        ),
    );

    let browser_block_id = registry
        .find_by_name("browser")
        .map(|e| e.manifest.id)
        .unwrap_or(BlockId::new(99));
    let mut browser_block = BrowserBlock::new(browser_block_id, BrowserConfig::default());
    let mut router = MessageRouter::new();
    router.register_handler(
        browser_block_id.0,
        Box::new(move |packet| browser_block.handle_message(packet)),
    );
    push_log(
        &logs,
        format!("AIOS: browser block '{}' ready", browser_block_id),
    );

    push_log(&logs, "AIOS: initializing access control...".into());
    let access_control = AccessControlLayer::new(b"aios_master_secret_2026".to_vec(), 86_400_000);

    push_log(&logs, "AIOS: initializing watchdog...".into());
    let watchdog = Watchdog::new(WatchdogConfig::default());

    push_log(&logs, "AIOS: initializing LLM engine...".into());
    let _llm = LlmEngine::from_config(default_config());

    push_log(&logs, "AIOS: initializing WASM executor...".into());
    let _executor = match BlockExecutor::with_default_config() {
        Ok(exec) => exec,
        Err(e) => {
            push_log(&logs, format!("AIOS WARN: WASM executor init failed: {e}"));
            push_log(&logs, "AIOS: creating fallback executor...".into());
            BlockExecutor::with_default_config().map_err(|e| format!("WASM init failed: {e}"))?
        }
    };

    let bridge = BridgeContext::new(scheduler, registry, access_control, watchdog, 42);
    let bridge = Arc::new(bridge);

    push_log(&logs, "AIOS: starting bridge server...".into());
    let bridge_running = Arc::new(AtomicBool::new(false));
    let bridge_clone = bridge.clone();
    let addr = format!("0.0.0.0:{}", config.bridge_port);
    let logs_clone = logs.clone();
    let br_flag = bridge_running.clone();

    tokio::spawn(async move {
        push_log(&logs_clone, format!("AIOS: Bridge listening on {addr}"));
        br_flag.store(true, Ordering::SeqCst);
        if let Err(e) = start_server(bridge_clone, &addr).await {
            push_log(
                &logs_clone,
                format!("AIOS ERROR: Bridge server failed: {e}"),
            );
            br_flag.store(false, Ordering::SeqCst);
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    push_log(&logs, "AIOS: all subsystems initialized.".into());

    Ok(OrchestratorState {
        hw_profile,
        bridge,
        router,
        browser_block_id,
        start_time: Instant::now(),
        bridge_running,
        logs,
    })
}
