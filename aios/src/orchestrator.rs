use crate::blocks;
use crate::hw_probe::{self, HwProfile};
use aios_block_mgr::registry::BlockRegistry;
use aios_block_mgr::router::MessageRouter;
use aios_bridge::server::{start_server, BridgeContext};
use aios_cluster::config::ClusterConfig;
use aios_cluster::executor::SchedulerProcessExecutor;
use aios_cluster::scheduler::DistributedScheduler;
use aios_cluster::transport::TcpClusterTransport;
use aios_cluster::types::{NodeInfo, NodeMetrics, NodeStatus};
use aios_core::block::BlockId;
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
    pub net_block_id: BlockId,
    /// Compiled-in base blocks that were wired at boot.
    pub base_blocks: Vec<blocks::BaseBlockInfo>,
    /// Result of the on-disk block phase.
    pub disk_blocks: blocks::DiskBlockReport,
    /// Executor that keeps on-disk WebAssembly blocks alive after bring-up and
    /// services their IPC traffic (`Arc<Mutex<..>>` because router handlers,
    /// which must be `Send`, share it).
    pub executor: Arc<Mutex<BlockExecutor>>,
    pub safe_mode: bool,
    pub start_time: Instant,
    pub bridge_running: Arc<AtomicBool>,
    pub logs: Arc<Mutex<Vec<String>>>,
    /// Distributed cluster scheduler when `AIOS_CLUSTER_PEERS` is configured.
    pub cluster: Option<Arc<Mutex<DistributedScheduler>>>,
}

pub struct AppConfig {
    pub bridge_port: u16,
    pub safe_mode: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bridge_port: 8080,
            safe_mode: false,
        }
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
    let scheduler = Arc::new(Mutex::new(
        Scheduler::new(total_ram_mb)
            .with_aging_threshold(5000)
            .with_time_slice(100)
            .with_max_restarts(5),
    ));

    push_log(&logs, "AIOS: initializing block registry...".into());
    let mut registry = BlockRegistry::new();

    // Infrastructure placeholders: present in the topology so the dependency
    // graph reflects boot order; they are not serviced over IPC.
    for (name, version, binary) in [
        ("ipc_bus", "1.0.0", &b"ipc_bus"[..]),
        ("scheduler", "1.0.0", &b"scheduler"[..]),
    ] {
        if let Ok(id) = registry.register_block(name, version, binary.to_vec()) {
            let _ = registry.activate_block(id);
        }
    }
    registry.set_block_dependencies("ipc_bus", vec!["hal".into()]);
    registry.set_block_dependencies("scheduler", vec!["ipc_bus".into()]);

    // Base set: compiled-in blocks created on demand by the block factory and
    // wired into the message router exactly like a disk block would be.
    let mut router = MessageRouter::new();
    let base_blocks = blocks::register_base_blocks(&mut registry, &mut router);
    push_log(
        &logs,
        format!("AIOS: wired {} base blocks", base_blocks.len()),
    );

    let net_block_id = registry
        .find_by_name("net_settings")
        .map(|e| e.manifest.id)
        .unwrap_or(BlockId::new(100));

    push_log(&logs, "AIOS: initializing WASM executor...".into());
    let executor = Arc::new(Mutex::new(
        BlockExecutor::with_default_config().map_err(|e| format!("WASM init failed: {e}"))?,
    ));

    // Disk set: only the blocks dropped in by the installer run, block by block.
    let blocks_dir = std::env::var("AIOS_BLOCKS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("blocks"));
    let disk_blocks = if config.safe_mode {
        push_log(
            &logs,
            "AIOS SAFE MODE: skipping disk blocks (block discovery disabled)".into(),
        );
        blocks::DiskBlockReport::default()
    } else {
        let report = blocks::load_disk_blocks(
            &mut registry,
            Arc::clone(&executor),
            &mut router,
            &blocks_dir,
        );
        let routed = report.loaded.iter().filter(|b| b.routed).count();
        push_log(
            &logs,
            format!(
                "AIOS: loaded {} disk blocks ({} routed over IPC, {} registered-only, {} failed) from {:?}",
                report.loaded_count(),
                routed,
                report.registered_only.len(),
                report.failed_count(),
                blocks_dir
            ),
        );
        report
    };
    push_log(
        &logs,
        format!("AIOS: block topology — {} blocks total", registry.count()),
    );

    push_log(&logs, "AIOS: initializing access control...".into());
    let access_control = AccessControlLayer::new(b"aios_master_secret_2026".to_vec(), 86_400_000);

    push_log(&logs, "AIOS: initializing watchdog...".into());
    let watchdog = Watchdog::new(WatchdogConfig::default());

    push_log(&logs, "AIOS: initializing LLM engine...".into());
    let _llm = LlmEngine::from_config(default_config());

    push_log(&logs, "AIOS: initializing cluster node...".into());
    let cluster = match ClusterConfig::from_env() {
        Some(cfg) => {
            push_log(
                &logs,
                format!(
                    "AIOS: cluster node {} ({}) starting, {} peers",
                    cfg.node_id,
                    cfg.addr,
                    cfg.peers.len()
                ),
            );
            let mut node = DistributedScheduler::new(
                NodeInfo {
                    id: cfg.node_id,
                    name: cfg.node_name.clone(),
                    addr: cfg.addr.clone(),
                    tier: cfg.tier,
                    status: NodeStatus::Online,
                    metrics: NodeMetrics::idle(),
                },
                Arc::new(TcpClusterTransport::new(&cfg.addr)),
                cfg.strategy,
            )
            .with_heartbeat(std::time::Duration::from_millis(cfg.heartbeat_ms))
            .with_failover_threshold(std::time::Duration::from_millis(cfg.failover_threshold_ms))
            .with_failover_respawn(cfg.failover_respawn)
            .with_checkpoint_ttl(std::time::Duration::from_millis(cfg.checkpoint_ttl_ms));
            node.set_executor(Arc::new(SchedulerProcessExecutor::new(
                cfg.node_id,
                scheduler.clone(),
            )));
            if let Err(e) = node.start(&cfg.peers) {
                push_log(&logs, format!("AIOS ERROR: cluster start failed: {e}"));
                None
            } else {
                let node = Arc::new(Mutex::new(node));
                let node_loop = node.clone();
                let logs_loop = logs.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    let events = node_loop.lock().map(|mut n| n.tick()).unwrap_or_default();
                    for event in events {
                        push_log(&logs_loop, format!("AIOS cluster: {event}"));
                    }
                });
                push_log(
                    &logs,
                    format!(
                        "AIOS: cluster node {} listening on {}",
                        cfg.node_id, cfg.addr
                    ),
                );
                Some(node)
            }
        }
        None => {
            push_log(
                &logs,
                "AIOS: clustering disabled (set AIOS_CLUSTER_PEERS to enable)".into(),
            );
            None
        }
    };

    let bridge = BridgeContext::new(scheduler, registry, access_control, watchdog, 42);
    let bridge = Arc::new(bridge);

    push_log(&logs, "AIOS: starting bridge server...".into());
    let bridge_running = Arc::new(AtomicBool::new(false));
    if config.safe_mode {
        push_log(
            &logs,
            "AIOS SAFE MODE: bridge server disabled — shell and local blocks only".into(),
        );
    } else {
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
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    push_log(&logs, "AIOS: all subsystems initialized.".into());

    Ok(OrchestratorState {
        hw_profile,
        bridge,
        router,
        net_block_id,
        base_blocks,
        disk_blocks,
        executor,
        safe_mode: config.safe_mode,
        start_time: Instant::now(),
        bridge_running,
        logs,
        cluster,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::block::StatefulBlock;
    use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
    use aios_net_config::block::NetSettingsBlock;
    use aios_net_config::config::NetworkConfig;

    fn build_net_router(dir: &tempfile::TempDir) -> (MessageRouter, BlockId) {
        let mut registry = BlockRegistry::new();
        let _ = registry.register_block("net_settings", "1.0.0", b"net-settings-native".to_vec());
        let id = registry
            .find_by_name("net_settings")
            .map(|e| e.manifest.id)
            .unwrap_or(BlockId::new(100));
        let mut net_block =
            NetSettingsBlock::new(id, NetworkConfig::default(), dir.path().join("net.json"));
        let mut router = MessageRouter::new();
        router.register_handler(
            id.0,
            Box::new(move |packet| net_block.handle_message(packet)),
        );
        (router, id)
    }

    fn dispatch_net(
        router: &mut MessageRouter,
        id: BlockId,
        command: &str,
        data: Vec<u8>,
    ) -> NetworkConfig {
        let packet = IpcPacket::new(
            0,
            id.0,
            CommandId::Custom,
            Payload::Custom(command.into(), data),
        );
        let resp = router.dispatch(&packet).unwrap().unwrap();
        let json = match &resp.payload {
            Payload::Text(t) => t.clone(),
            other => panic!("expected text response, got {other:?}"),
        };
        NetworkConfig::from_json(&json).unwrap()
    }

    #[test]
    fn test_net_settings_registered_in_registry() {
        let mut registry = BlockRegistry::new();
        let _ = registry.register_block("net_settings", "1.0.0", b"net-settings-native".to_vec());
        assert!(registry.find_by_name("net_settings").is_some());
    }

    #[test]
    fn test_net_get_routed_over_ipc() {
        let dir = tempfile::tempdir().unwrap();
        let (mut router, id) = build_net_router(&dir);
        let config = dispatch_net(&mut router, id, "net_get", Vec::new());
        assert_eq!(config.hostname, "aios-host");
    }

    #[test]
    fn test_net_set_routed_over_ipc() {
        let dir = tempfile::tempdir().unwrap();
        let (mut router, id) = build_net_router(&dir);
        let updates = serde_json::json!({ "hostname": "kernel-host", "listen_port": 9090 });
        let config = dispatch_net(&mut router, id, "net_set", updates.to_string().into_bytes());
        assert_eq!(config.hostname, "kernel-host");
        assert_eq!(config.listen_port, 9090);
    }

    #[test]
    fn test_net_reset_routed_over_ipc() {
        let dir = tempfile::tempdir().unwrap();
        let (mut router, id) = build_net_router(&dir);
        let updates = serde_json::json!({ "hostname": "temporary" });
        let _ = dispatch_net(&mut router, id, "net_set", updates.to_string().into_bytes());
        let config = dispatch_net(&mut router, id, "net_reset", Vec::new());
        assert_eq!(config.hostname, "aios-host");
    }
}
