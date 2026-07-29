use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_context::stability::StabilityScore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::TelemetryEntry;
use aios_core::crypto;
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::{HalBlock, HardwareProfile};
use aios_ipc::bus::IpcBus;
use aios_live_update::wasm_engine::{SwapParams, WasmLiveUpdateEngine};
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use aios_security::access_control::AccessControlLayer;
use aios_security::capability::Capability;
use aios_wasm::executor::BlockExecutor;
use aios_wasm::isolation::IsolationConfig;
use aios_watchdog::heartbeat::Heartbeat;
use aios_watchdog::runner::WatchdogRunner;
use aios_watchdog::watchdog::{Watchdog, WatchdogConfig};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn test_full_system_real_components() {
    let profile = HardwareProfile::mock_modern();
    let ai_tier = AiTier::from_profile(&profile);
    assert_eq!(ai_tier, AiTier::Tier1);

    let mut registry = BlockRegistry::new();
    let hal_data = b"hal-native";
    let hal =
        BlockLoader::load_from_binary(&mut registry, "hal", "1.0.0", hal_data.to_vec()).unwrap();
    assert_eq!(hal.name, "hal");

    let mut bus = IpcBus::new(256);
    let pkt = IpcPacket::new(0, hal.id.0, CommandId::HealthCheck, Payload::HealthCheck);
    bus.send(pkt).unwrap();
    let received = bus.receive();
    assert!(received.is_some());

    let mut scheduler = Scheduler::new(2048);
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let pid = scheduler
        .spawn_real_process("main_loop", Priority::High, 64, move |_term, _susp| {
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

    std::thread::sleep(Duration::from_millis(50));
    assert!(counter.load(Ordering::SeqCst) >= 1);

    let mut score = StabilityScore::new("hal", "1.0.0");
    assert!(score.is_healthy());

    let mut ctx_store = EmbeddedContextStore::new(1000);
    ctx_store
        .telemetry_mut()
        .record(TelemetryEntry::new("system.boot", 1.0, 64));

    let mut acl = AccessControlLayer::new(b"test_secret".to_vec(), 60_000);
    let token = acl.issue_token(1, vec![Capability::BlockLoad]).unwrap();
    assert!(token.has_capability(&Capability::BlockLoad));

    scheduler.kill_process(pid).unwrap();
    assert_eq!(scheduler.process_count(), 0);

    let hal = registry.get(hal.id).unwrap();
    assert_eq!(hal.manifest.name, "hal");
}

#[test]
fn test_wasm_block_lifecycle_deploy_swap_rollback() {
    let mut registry = BlockRegistry::new();

    let v1 = r#"
        (module
            (func (export "init"))
            (func (export "version") (result i32) i32.const 1)
        )
    "#
    .as_bytes();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "lifecycle", "1.0.0", v1.to_vec()).unwrap();

    let mut executor = BlockExecutor::with_default_config().unwrap();
    let result = executor
        .execute_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();
    assert!(result.success);
    assert!(result.functions_called.contains(&"init".to_string()));

    let r = executor
        .call_block_func(manifest.id, "version", &[])
        .unwrap();
    assert_eq!(r[0].i32(), Some(1));

    let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
    let mut queue = IpcBus::new(256);
    engine
        .deploy_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();

    let v2 = r#"
        (module
            (func (export "init"))
            (func (export "version") (result i32) i32.const 2)
        )
    "#
    .as_bytes();
    engine
        .swap_block(
            &mut registry,
            manifest.id,
            SwapParams {
                new_binary: v2.to_vec(),
                new_version: "2.0.0".to_string(),
                health_check: None,
                isolation: IsolationConfig::default(),
            },
            &mut queue,
        )
        .unwrap();

    let r = engine.call_block_func(manifest.id, "version", &[]).unwrap();
    assert_eq!(r[0].i32(), Some(2));

    let rollback = engine.rollback_block(manifest.id, &mut queue).unwrap();
    assert_eq!(rollback.restored_version, "1.0.0");
}

#[test]
fn test_watchdog_scheduler_ipc_combined() {
    let config = WatchdogConfig {
        heartbeat_interval_ms: 50,
        max_missed_heartbeats: 3,
        warn_threshold: 2,
        recovery_timeout_ms: 500,
        secret: b"test_secret".to_vec(),
    };

    let mut _watchdog = Watchdog::new(config.clone());
    let mut runner = WatchdogRunner::start(config);

    let mut scheduler = Scheduler::new(2048);
    let mut bus = IpcBus::new(256);

    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let pid = scheduler
        .spawn_real_process("ipc_worker", Priority::Normal, 32, move |_term, _susp| {
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

    std::thread::sleep(Duration::from_millis(80));
    assert!(counter.load(Ordering::SeqCst) >= 1);

    let pkt = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::HealthCheck);
    bus.send(pkt).unwrap();
    let received = bus.receive();
    assert!(received.is_some());

    let hb = Heartbeat::new(1, b"test_secret");
    _watchdog.receive_heartbeat(&hb).unwrap();

    std::thread::sleep(Duration::from_millis(100));

    scheduler.kill_process(pid).unwrap();
    runner.stop();
}

#[test]
fn test_crypto_ipc_bus_end_to_end() {
    let data = b"confidential payload";
    let hash = crypto::compute_sha256_bytes(data);
    assert!(crypto::verify_sha256_bytes(data, &hash));

    let hex_hash = crypto::compute_sha256(data);
    assert!(crypto::verify_sha256(data, &hex_hash));

    let mut bus = IpcBus::new(128);
    for i in 0..10 {
        let pkt = IpcPacket::new(
            0,
            1,
            CommandId::HealthCheck,
            Payload::Binary(format!("pkt_{}", i).into_bytes()),
        );
        bus.send(pkt).unwrap();
    }

    let mut received_count = 0;
    while bus.receive().is_some() {
        received_count += 1;
    }
    assert_eq!(received_count, 10);
}

#[test]
fn test_scheduler_priority_aging_real_threads() {
    let mut s = Scheduler::new(4096);

    let low_done = Arc::new(AtomicBool::new(false));
    let ld = low_done.clone();
    let _low = s
        .spawn_real_process("low_worker", Priority::Low, 32, move |term, _susp| {
            while !term.should_stop() {
                std::thread::sleep(Duration::from_millis(5));
            }
            ld.store(true, Ordering::SeqCst);
        })
        .unwrap();

    std::thread::sleep(Duration::from_millis(10));

    let bg = s
        .spawn_process("bg_task", Priority::Background, 32)
        .unwrap();
    s.set_last_scheduled(bg, aios_process_mgr::task::now_ms() - 2000);

    assert_eq!(s.real_thread_count(), 1);
    assert_eq!(s.process_count(), 2);
}

#[test]
fn test_full_disk_to_wasm_execution() {
    let dir = tempfile::tempdir().unwrap();

    let wasm = r#"
        (module
            (func (export "init"))
            (func (export "start"))
            (func (export "fibonacci") (param i32) (result i32)
                (if (result i32) (i32.le_s (local.get 0) (i32.const 1))
                    (then (local.get 0))
                    (else
                        (i32.add
                            (call 2 (i32.sub (local.get 0) (i32.const 1)))
                            (call 2 (i32.sub (local.get 0) (i32.const 2))))))))
    "#
    .as_bytes();
    std::fs::write(dir.path().join("mathlib_1.0.0.wasm"), wasm).unwrap();

    let mut registry = BlockRegistry::new();
    let mut executor = BlockExecutor::with_default_config().unwrap();
    let results =
        executor.load_from_path_and_execute(&mut registry, dir.path(), IsolationConfig::default());

    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());

    let entry = registry.find_by_name("mathlib").unwrap();
    let r = executor
        .call_block_func(entry.manifest.id, "fibonacci", &[wasmtime::Val::I32(10)])
        .unwrap();
    assert_eq!(r[0].i32(), Some(55));
}

#[test]
fn test_stability_and_acl_combined() {
    let mut score = StabilityScore::new("test_block", "1.0.0");
    score.record_uptime(10_000);
    assert!(score.is_healthy());

    let mut acl = AccessControlLayer::new(b"secret".to_vec(), 60_000);
    let token = acl
        .issue_token(1, vec![Capability::FsRead, Capability::FsWrite])
        .unwrap();
    assert!(token.has_capability(&Capability::FsRead));
    assert!(token.has_capability(&Capability::FsWrite));
    assert!(!token.is_expired());
}
