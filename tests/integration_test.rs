use aios_block_mgr::dependency::DependencyGraph;
use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_block_mgr::router::MessageRouter;
use aios_block_mgr::version::SemanticVersion;
use aios_context::stability::StabilityScore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::TelemetryEntry;
use aios_core::block::{BlockId, StatefulBlock};
use aios_core::crypto;
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::{HalBlock, HardwareProfile};
use aios_ipc::bus::{BackpressurePolicy, IpcBus};
use aios_live_update::engine::LiveUpdateEngine;
use aios_process_mgr::process_control::handle_process_command;
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::{Priority, ProcessId};
use aios_security::access_control::AccessControlLayer;
use aios_security::capability::Capability;
use aios_security::sandbox::{Sandbox, SandboxState};
use aios_watchdog::heartbeat::Heartbeat;
use aios_watchdog::safe_mode::SafeModeShell;
use aios_watchdog::watchdog::{Watchdog, WatchdogAction, WatchdogConfig, WatchdogState};

// ============================================================
// Test 1: Full system lifecycle
// ============================================================
#[test]
fn test_full_system_lifecycle() {
    // 1. Initialize HAL
    let profile = HardwareProfile::mock_modern();
    let ai_tier = AiTier::from_profile(&profile);
    assert_eq!(ai_tier, AiTier::Tier1);

    // 2. Initialize Block Manager
    let mut registry = BlockRegistry::new();
    let hal_data = b"hal-native";
    let hal_manifest =
        BlockLoader::load_from_binary(&mut registry, "hal", "1.0.0", hal_data.to_vec()).unwrap();
    assert_eq!(hal_manifest.name, "hal");

    // 3. Register more blocks
    let _ = BlockLoader::load_from_binary(&mut registry, "ipc_bus", "1.0.0", b"ipc_bus".to_vec())
        .unwrap();
    let _ =
        BlockLoader::load_from_binary(&mut registry, "scheduler", "1.0.0", b"scheduler".to_vec())
            .unwrap();
    assert_eq!(registry.count(), 3);

    // 4. Initialize Scheduler
    let mut scheduler = Scheduler::new(profile.memory.total_mb);
    let pid1 = scheduler
        .spawn_process("ai_orchestrator", Priority::High, 512)
        .unwrap();
    let pid2 = scheduler
        .spawn_process("io_handler", Priority::Normal, 128)
        .unwrap();
    let pid3 = scheduler
        .spawn_process("monitor", Priority::Low, 64)
        .unwrap();
    assert_eq!(scheduler.process_count(), 3);

    // 5. Schedule and run
    let scheduled = scheduler.schedule_next().unwrap();
    assert_eq!(scheduled, pid1); // High priority first

    // 6. Priority change
    scheduler.set_priority(pid3, Priority::Critical).unwrap();
    assert_eq!(
        scheduler.get_process(pid3).unwrap().priority,
        Priority::Critical
    );

    // 7. Kill process
    scheduler.kill_process(pid2).unwrap();
    assert_eq!(scheduler.process_count(), 2);

    // 8. Check topology
    let topo = registry.topology();
    assert_eq!(topo.len(), 3);

    // 9. Verify block signatures
    for id in registry.all_ids() {
        assert!(registry.verify_signature(id).unwrap());
    }
}

// ============================================================
// Test 2: IPC Protocol serialization speed
// ============================================================
#[test]
fn test_ipc_serialization_speed() {
    let pkt = IpcPacket::new(
        0,
        1,
        CommandId::SpawnProcess,
        Payload::SpawnProcess {
            name: "speed_test".into(),
            priority: 2,
            ram_mb: 256,
        },
    );

    let start = std::time::Instant::now();
    let iterations = 50_000;
    for _ in 0..iterations {
        let bytes = pkt.serialize().unwrap();
        let _ = IpcPacket::deserialize(&bytes).unwrap();
    }
    let elapsed = start.elapsed();
    let per_us = elapsed.as_micros() as f64 / iterations as f64;
    println!("IPC roundtrip: {per_us:.3} us/packet ({iterations} iterations)");
    let threshold = if cfg!(debug_assertions) { 50.0 } else { 2.0 };
    assert!(
        per_us < threshold,
        "IPC serialization too slow: {per_us} us (threshold: {threshold})"
    );
}

// ============================================================
// Test 3: Concurrent process spawn stress test
// ============================================================
#[test]
fn test_concurrent_process_spawns() {
    let mut scheduler = Scheduler::new(65536);
    let mut pids = Vec::new();

    // Spawn 100 processes
    for i in 0..100 {
        let prio = match i % 4 {
            0 => Priority::Critical,
            1 => Priority::High,
            2 => Priority::Normal,
            _ => Priority::Low,
        };
        let pid = scheduler
            .spawn_process(&format!("process_{i}"), prio, 64)
            .unwrap();
        pids.push(pid);
    }

    assert_eq!(scheduler.process_count(), 100);
    assert_eq!(scheduler.ram_usage().0, 6400);

    // Kill all
    for pid in pids {
        scheduler.kill_process(pid).unwrap();
    }
    assert_eq!(scheduler.process_count(), 0);
    assert_eq!(scheduler.ram_usage().0, 0);
}

// ============================================================
// Test 4: Live-update with concurrent IPC traffic
// ============================================================
#[test]
fn test_live_update_with_concurrent_ipc() {
    let mut engine = LiveUpdateEngine::new(5000);
    let mut bus = IpcBus::new(1024);

    // Fill bus with messages
    for i in 0..50 {
        bus.send(IpcPacket::new(
            0,
            i as u32,
            CommandId::HealthCheck,
            Payload::Binary(format!("msg_{i}").into_bytes()),
        ))
        .unwrap();
    }
    assert_eq!(bus.len(), 50);

    // Perform swap while bus has traffic
    let new_bin = b"updated_module_v2";
    let hash = crypto::compute_sha256_bytes(new_bin);

    engine
        .perform_swap(
            1,
            b"old_module".to_vec(),
            "1.0.0".into(),
            b"state_data".to_vec(),
            new_bin.to_vec(),
            "2.0.0".into(),
            hash,
            &mut bus,
            None,
        )
        .unwrap();

    // Bus should have all 50 messages restored
    assert_eq!(bus.len(), 50);

    // Verify messages are intact
    let first = bus.receive().unwrap();
    assert_eq!(first.header.target_block, 0);
}

// ============================================================
// Test 5: Crash resilience — scheduler survives crashes
// ============================================================
#[test]
fn test_crash_resilience() {
    let mut scheduler = Scheduler::new(8192).with_max_restarts(3);

    // Spawn a process that will "crash" repeatedly
    let pid = scheduler
        .spawn_process("fragile_worker", Priority::Normal, 256)
        .unwrap();

    // First crash — should be restartable
    let event = scheduler.report_crash(pid).unwrap();
    assert_eq!(event.crash_count, 1);
    assert!(scheduler.should_restart(pid));
    assert_eq!(scheduler.crash_log().len(), 1);

    // Second crash
    let event = scheduler.report_crash(pid).unwrap();
    assert_eq!(event.crash_count, 2);
    assert!(scheduler.should_restart(pid));

    // Third crash — max reached, no more restarts
    let event = scheduler.report_crash(pid).unwrap();
    assert_eq!(event.crash_count, 3);
    assert!(!scheduler.should_restart(pid));

    // Scheduler should still be operational with other processes
    let pid2 = scheduler
        .spawn_process("healthy_worker", Priority::High, 128)
        .unwrap();
    assert_eq!(scheduler.process_count(), 2);

    let scheduled = scheduler.schedule_next().unwrap();
    assert_eq!(scheduled, pid2); // healthy worker gets scheduled
}

// ============================================================
// Test 6: Message router dispatches correctly
// ============================================================
#[test]
fn test_message_router_integration() {
    let mut router = MessageRouter::new();

    // Register handlers
    router.register_handler(
        1,
        Box::new(|pkt| {
            Ok(Some(IpcPacket::response_ok(
                1,
                pkt.header.source_block,
                pkt.header.packet_id,
                Payload::Text("block_1_response".into()),
            )))
        }),
    );

    router.register_handler(
        2,
        Box::new(|pkt| {
            Ok(Some(IpcPacket::response_ok(
                2,
                pkt.header.source_block,
                pkt.header.packet_id,
                Payload::Text("block_2_response".into()),
            )))
        }),
    );

    // Route block_10 → block_2
    router.add_route(10, 20);
    router.register_handler(
        20,
        Box::new(|pkt| {
            Ok(Some(IpcPacket::response_ok(
                20,
                pkt.header.source_block,
                pkt.header.packet_id,
                Payload::Text("redirected_response".into()),
            )))
        }),
    );

    // Direct dispatch
    let pkt = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::Empty);
    let resp = router.dispatch(&pkt).unwrap().unwrap();
    assert_eq!(resp.payload, Payload::Text("block_1_response".into()));

    // Redirected dispatch
    let pkt = IpcPacket::new(0, 10, CommandId::HealthCheck, Payload::Empty);
    let resp = router.dispatch(&pkt).unwrap().unwrap();
    assert_eq!(resp.payload, Payload::Text("redirected_response".into()));
}

// ============================================================
// Test 7: Process control via IPC
// ============================================================
#[test]
fn test_process_control_ipc_integration() {
    let mut scheduler = Scheduler::new(4096);

    // Spawn via IPC
    let pkt = IpcPacket::new(
        0,
        3,
        CommandId::SpawnProcess,
        Payload::SpawnProcess {
            name: "ipc_spawned".into(),
            priority: 3,
            ram_mb: 128,
        },
    );
    let resp = handle_process_command(&mut scheduler, &pkt)
        .unwrap()
        .unwrap();
    assert!(matches!(resp.payload, Payload::Text(_)));

    // Extract PID from response text
    if let Payload::Text(ref text) = resp.payload {
        let pid: u64 = text.parse().unwrap();

        // Adjust priority via IPC
        let pkt = IpcPacket::new(
            0,
            3,
            CommandId::AdjustPriority,
            Payload::AdjustPriority {
                pid,
                new_priority: 1,
            },
        );
        handle_process_command(&mut scheduler, &pkt).unwrap();
        assert_eq!(
            scheduler.get_process(ProcessId::new(pid)).unwrap().priority,
            Priority::Low
        );

        // Kill via IPC
        let pkt = IpcPacket::new(0, 3, CommandId::KillProcess, Payload::KillProcess { pid });
        handle_process_command(&mut scheduler, &pkt).unwrap();
        assert_eq!(scheduler.process_count(), 0);
    }
}

// ============================================================
// Test 8: AI Tier classification across all hardware profiles
// ============================================================
#[test]
fn test_ai_tier_all_profiles() {
    // Modern workstation → Tier 1
    let modern = HardwareProfile::mock_modern();
    assert_eq!(AiTier::from_profile(&modern), AiTier::Tier1);

    // Legacy with AVX2 → Tier 2
    let legacy = HardwareProfile::mock_legacy();
    assert_eq!(AiTier::from_profile(&legacy), AiTier::Tier2);

    // Ancient 2012 CPU → Tier 3
    let ancient = HardwareProfile::mock_legacy_2012();
    assert_eq!(AiTier::from_profile(&ancient), AiTier::Tier3);
}

// ============================================================
// Test 9: Stateful block extract/restore
// ============================================================
#[test]
fn test_stateful_block_roundtrip() {
    let profile = HardwareProfile::mock_modern();
    let block = HalBlock::with_profile(BlockId::new(0), profile);

    // Extract state
    let state = block.extract_state().unwrap();
    assert!(!state.is_empty());

    // Create new block and restore
    let profile2 = HardwareProfile::mock_legacy();
    let mut block2 = HalBlock::with_profile(BlockId::new(1), profile2);
    assert_ne!(block2.profile().cpu.cores, block.profile().cpu.cores);

    block2.restore_state(&state).unwrap();
    assert_eq!(block2.profile().cpu.cores, block.profile().cpu.cores);
}

// ============================================================
// Test 10: Full hot-swap lifecycle with rollback
// ============================================================
#[test]
fn test_full_hotswap_lifecycle() {
    let mut engine = LiveUpdateEngine::new(5000);
    let mut bus = IpcBus::new(100);
    let mut registry = BlockRegistry::new();

    // Register original block
    let old_binary = b"original_module".to_vec();
    let id = registry
        .register_block("swap_target", "1.0.0", old_binary.clone())
        .unwrap();

    // Add traffic to bus
    for i in 0..20 {
        bus.send(IpcPacket::new(0, i, CommandId::HealthCheck, Payload::Empty))
            .unwrap();
    }

    // Perform hot-swap
    let new_binary = b"updated_module".to_vec();
    let hash = crypto::compute_sha256_bytes(&new_binary);
    engine
        .perform_swap(
            id.0,
            old_binary.clone(),
            "1.0.0".into(),
            b"block_state".to_vec(),
            new_binary.clone(),
            "2.0.0".into(),
            hash,
            &mut bus,
            None,
        )
        .unwrap();

    // Verify bus is restored
    assert_eq!(bus.len(), 20);

    // Verify rollback available
    assert!(engine.has_rollback(id.0));

    // Perform rollback
    let entry = engine.rollback(id.0, &mut bus).unwrap();
    assert_eq!(entry.old_binary, old_binary);
    assert_eq!(entry.old_version, "1.0.0");

    // Verify bus still intact after rollback
    assert_eq!(bus.len(), 20);
}

// ============================================================
// Test 11: Watchdog heartbeat lifecycle
// ============================================================
#[test]
fn test_watchdog_heartbeat_lifecycle() {
    let config = WatchdogConfig {
        heartbeat_interval_ms: 100,
        max_missed_heartbeats: 3,
        warn_threshold: 2,
        recovery_timeout_ms: 500,
        secret: b"integration_secret".to_vec(),
    };
    let mut watchdog = Watchdog::new(config);

    // Send heartbeat
    let hb = Heartbeat::new(1, b"integration_secret");
    watchdog.receive_heartbeat(&hb).unwrap();
    assert_eq!(watchdog.state(), WatchdogState::Monitoring);

    // Verify heartbeat stats
    let (received, missed) = watchdog.stats();
    assert_eq!(received, 1);
    assert_eq!(missed, 0);

    // Wait for missed heartbeats to trigger suspend
    for _ in 0..2 {
        std::thread::sleep(std::time::Duration::from_millis(120));
        watchdog.check_timeout();
    }
    std::thread::sleep(std::time::Duration::from_millis(120));
    let action = watchdog.check_timeout();
    assert_eq!(action, WatchdogAction::SuspendOrchestrator);
    assert_eq!(watchdog.state(), WatchdogState::Suspended);

    // Recovery attempt
    let action = watchdog.check_timeout();
    assert_eq!(action, WatchdogAction::KillProcess(0));
    assert_eq!(watchdog.state(), WatchdogState::Recovering);

    // Send valid heartbeat during recovery
    let hb2 = Heartbeat::new(2, b"integration_secret");
    watchdog.receive_heartbeat(&hb2).unwrap();
    assert_eq!(watchdog.state(), WatchdogState::Monitoring);

    // Verify log
    assert!(watchdog.state_log().len() >= 5);
}

// ============================================================
// Test 12: Safe mode shell full lifecycle
// ============================================================
#[test]
fn test_safe_mode_shell_lifecycle() {
    let mut sched = aios_process_mgr::scheduler::Scheduler::new(8192);
    let mut reg = aios_block_mgr::registry::BlockRegistry::new();
    let _ = sched.spawn_process(
        "ai_orchestrator",
        aios_process_mgr::task::Priority::High,
        512,
    );
    let _ = sched.spawn_process("io_handler", aios_process_mgr::task::Priority::Normal, 128);
    let _ = aios_block_mgr::loader::BlockLoader::load_from_binary(
        &mut reg,
        "hal",
        "0.1.0",
        b"hal-data".to_vec(),
    );

    let mut shell = SafeModeShell::new(2);

    // System status in safe mode
    let resp = shell.execute(SafeModeShell::parse_command("status"), &mut sched, &mut reg);
    assert!(resp.success);
    assert!(resp.output.contains("SAFE MODE"));
    assert!(resp.output.contains("Processes:"));

    // List processes
    let resp = shell.execute(SafeModeShell::parse_command("ps"), &mut sched, &mut reg);
    assert!(resp.success);
    assert!(resp.output.contains("ai_orchestrator"));

    // Restart orchestrator (attempt 1)
    let resp = shell.execute(
        SafeModeShell::parse_command("restart"),
        &mut sched,
        &mut reg,
    );
    assert!(resp.success);
    assert!(resp.output.contains("1/2"));

    // Restart orchestrator (attempt 2)
    let resp = shell.execute(
        SafeModeShell::parse_command("restart"),
        &mut sched,
        &mut reg,
    );
    assert!(resp.success);
    assert!(resp.output.contains("2/2"));

    // Restart orchestrator (attempt 3 — should fail)
    let resp = shell.execute(
        SafeModeShell::parse_command("restart"),
        &mut sched,
        &mut reg,
    );
    assert!(!resp.success);
    assert!(resp.output.contains("Max restarts"));

    // View logs
    let resp = shell.execute(SafeModeShell::parse_command("logs"), &mut sched, &mut reg);
    assert!(resp.success);
    assert!(resp.output.contains("Orchestrator restart"));
}

// ============================================================
// Test 13: Security capability token + sandbox enforcement
// ============================================================
#[test]
fn test_security_sandbox_enforcement() {
    // Create access control layer
    let mut acl = AccessControlLayer::new(b"test_key".to_vec(), 60_000);

    // Issue token to block 1 with limited capabilities
    acl.issue_token(1, vec![Capability::FsRead, Capability::FsWrite])
        .unwrap();

    // Block 1 can read/write files
    assert!(acl.check_permission(1, &Capability::FsRead).is_ok());
    assert!(acl.check_permission(1, &Capability::FsWrite).is_ok());

    // Block 1 cannot bind to network
    assert!(acl.check_permission(1, &Capability::NetBind).is_err());

    // Create sandbox from token
    let token = acl.get_token(1).unwrap();
    let mut sandbox = Sandbox::from_token(token, 1024 * 1024, 100);
    sandbox.start();

    // Allowed syscalls
    assert!(sandbox.check_syscall("read", &Capability::FsRead));
    assert!(sandbox.check_syscall("write", &Capability::FsWrite));

    // Blocked syscall — triggers violation
    assert!(!sandbox.check_syscall("bind", &Capability::NetBind));
    assert!(sandbox.is_violated());
    assert_eq!(sandbox.state(), SandboxState::Violated);

    // ACL recorded violation
    assert!(!acl.try_check_permission(1, &Capability::NetBind));
    assert_eq!(acl.violation_count(), 1);
}

// ============================================================
// Test 14: Context store cross-collection integration
// ============================================================
#[test]
fn test_context_store_cross_collection() {
    let mut store = EmbeddedContextStore::new(1000);

    // Record telemetry
    store.telemetry_mut().record(
        TelemetryEntry::new("cpu", 75.0, 2048)
            .with_block(1)
            .with_process("ai_orchestrator"),
    );
    store.telemetry_mut().record(
        TelemetryEntry::new("cpu", 85.0, 4096)
            .with_block(1)
            .with_process("ai_orchestrator"),
    );

    // Record workflow
    store
        .workflows_mut()
        .record("video_editing".into(), vec!["render_block".into()]);
    store
        .workflows_mut()
        .get_mut("video_editing")
        .unwrap()
        .set_priority("ai_orchestrator", 4);

    // Record stability
    store
        .stability_mut()
        .record(StabilityScore::new("render_block", "2.0.0"));
    store
        .stability_mut()
        .record(StabilityScore::new("render_block", "1.0.0"));
    store.stability_mut().scores[1].score = 0.4;

    // Verify cross-collection queries
    assert_eq!(store.telemetry().average_value("cpu"), Some(80.0));
    assert_eq!(store.telemetry().peak_ram(), 4096);
    assert_eq!(store.telemetry().query_by_block(1).len(), 2);

    let workflow = store.workflows().get("video_editing").unwrap();
    assert_eq!(workflow.get_priority("ai_orchestrator"), Some(4));
    assert_eq!(workflow.usage_count, 1);

    let best = store.stability().best_version("render_block").unwrap();
    assert_eq!(best.binary_version, "2.0.0");

    // Export summary
    let export = store.export_all();
    assert_eq!(export.telemetry_entries, 2);
    assert_eq!(export.workflow_entries, 1);
    assert_eq!(export.stability_entries, 2);
    assert_eq!(store.total_entries(), 5);
}

// ============================================================
// Test 15: Watchdog + Scheduler crash coordination
// ============================================================
#[test]
fn test_watchdog_scheduler_crash_coordination() {
    let mut scheduler = Scheduler::new(8192);
    let config = WatchdogConfig {
        heartbeat_interval_ms: 100,
        max_missed_heartbeats: 3,
        warn_threshold: 2,
        recovery_timeout_ms: 500,
        secret: b"coord_secret".to_vec(),
    };
    let mut watchdog = Watchdog::new(config);

    // Spawn processes
    let pid1 = scheduler
        .spawn_process("ai_orchestrator", Priority::Critical, 1024)
        .unwrap();
    let _pid2 = scheduler
        .spawn_process("io_handler", Priority::Normal, 512)
        .unwrap();

    // Orchestrator is running, watchdog gets heartbeats
    let hb = Heartbeat::new(1, b"coord_secret");
    watchdog.receive_heartbeat(&hb).unwrap();
    assert_eq!(watchdog.state(), WatchdogState::Monitoring);

    // Simulate orchestrator crash
    scheduler.report_crash(pid1).unwrap();
    assert!(scheduler.should_restart(pid1));

    // Watchdog detects missed heartbeats
    for _ in 0..2 {
        std::thread::sleep(std::time::Duration::from_millis(120));
        watchdog.check_timeout();
    }
    std::thread::sleep(std::time::Duration::from_millis(120));
    let action = watchdog.check_timeout();
    assert_eq!(action, WatchdogAction::SuspendOrchestrator);

    // Scheduler still operational after orchestrator crash
    assert_eq!(scheduler.process_count(), 2);
    let running = scheduler.process_count();
    assert!(running >= 1);

    // Recovery: new heartbeat arrives
    watchdog.check_timeout(); // → Recovering
    let hb2 = Heartbeat::new(2, b"coord_secret");
    watchdog.receive_heartbeat(&hb2).unwrap();
    assert_eq!(watchdog.state(), WatchdogState::Monitoring);
}

// ============================================================
// Test 16: Security + IPC packet capability check
// ============================================================
#[test]
fn test_security_ipc_packet_capability_check() {
    let mut acl = AccessControlLayer::new(b"ipc_secret".to_vec(), 60_000);

    // Block 1: can spawn processes
    acl.issue_token(1, vec![Capability::ProcessSpawn, Capability::FsRead])
        .unwrap();

    // Block 2: can only read files
    acl.issue_token(2, vec![Capability::FsRead]).unwrap();

    // Construct IPC packets
    let spawn_pkt = IpcPacket::new(
        1,
        0,
        CommandId::SpawnProcess,
        Payload::SpawnProcess {
            name: "test_proc".into(),
            priority: 2,
            ram_mb: 256,
        },
    );

    // Block 1 can send spawn command
    assert!(acl.check_permission(1, &Capability::ProcessSpawn).is_ok());
    assert_eq!(spawn_pkt.header.source_block, 1);

    // Block 2 cannot send spawn command
    assert!(acl.check_permission(2, &Capability::ProcessSpawn).is_err());

    // Both blocks can read
    assert!(acl.check_permission(1, &Capability::FsRead).is_ok());
    assert!(acl.check_permission(2, &Capability::FsRead).is_ok());
}

// ============================================================
// Test 17: Live update with security token revocation
// ============================================================
#[test]
fn test_live_update_with_security_revocation() {
    let mut acl = AccessControlLayer::new(b"swap_key".to_vec(), 60_000);
    let mut engine = LiveUpdateEngine::new(30_000);
    let mut bus = IpcBus::new(1024);

    let block_id: u32 = 1;

    // Issue token for block
    acl.issue_token(block_id, vec![Capability::BlockLoad, Capability::FsWrite])
        .unwrap();
    assert!(acl
        .check_permission(block_id, &Capability::BlockLoad)
        .is_ok());

    // Prepare old and new binaries
    let old_binary = vec![1u8; 100];
    let new_binary = vec![2u8; 100];
    let new_sha256 = crypto::compute_sha256_bytes(&new_binary);

    // Fill bus with messages
    for _i in 0..10 {
        bus.send(IpcPacket::new(
            0,
            block_id,
            CommandId::HealthCheck,
            Payload::HealthCheck,
        ))
        .unwrap();
    }

    // Perform hot-swap
    engine
        .perform_swap(
            block_id,
            old_binary.clone(),
            "1.0.0".into(),
            vec![10, 20, 30],
            new_binary.clone(),
            "2.0.0".into(),
            new_sha256,
            &mut bus,
            None,
        )
        .unwrap();

    // Bus restored
    assert_eq!(bus.len(), 10);

    // Revoke token after swap
    acl.revoke_token(block_id);
    assert!(acl
        .check_permission(block_id, &Capability::BlockLoad)
        .is_err());

    // Rollback still works (engine has its own rollback data)
    let entry = engine.rollback(block_id, &mut bus).unwrap();
    assert_eq!(entry.old_binary, old_binary);
    assert_eq!(entry.old_version, "1.0.0");
}

// ============================================================
// Test 18: Telemetry-driven scheduler priority adjustment
// ============================================================
#[test]
fn test_telemetry_driven_priority_adjustment() {
    let mut store = EmbeddedContextStore::new(1000);
    let mut scheduler = Scheduler::new(8192);

    // Spawn processes
    let pid_render = scheduler
        .spawn_process("render_worker", Priority::Normal, 512)
        .unwrap();
    let _pid_io = scheduler
        .spawn_process("io_handler", Priority::Low, 256)
        .unwrap();

    // Record telemetry showing render_worker needs high priority
    store.telemetry_mut().record(
        TelemetryEntry::new("cpu", 95.0, 4096)
            .with_block(1)
            .with_process("render_worker"),
    );

    // Record workflow preference
    store
        .workflows_mut()
        .record("video_render".into(), vec!["render_block".into()]);
    store
        .workflows_mut()
        .get_mut("video_render")
        .unwrap()
        .set_priority("render_worker", 4);

    // Apply learned priority
    if let Some(workflow) = store.workflows().get("video_render") {
        if let Some(prio) = workflow.get_priority("render_worker") {
            let priority = Priority::from_u8(prio);
            scheduler.set_priority(pid_render, priority).unwrap();
        }
    }

    // Verify priority was adjusted
    let proc = scheduler.get_process(pid_render).unwrap();
    assert_eq!(proc.priority, Priority::Critical);

    // Scheduler still functional
    let next = scheduler.schedule_next().unwrap();
    assert_eq!(next, pid_render);
}

#[test]
fn test_scheduler_aging_starvation_prevention() {
    let mut scheduler = Scheduler::new(8192).with_aging_threshold(200);

    let _low1 = scheduler
        .spawn_process("low_bg", Priority::Low, 64)
        .unwrap();
    let low2 = scheduler
        .spawn_process("low_bg2", Priority::Low, 64)
        .unwrap();
    let high = scheduler
        .spawn_process("critical", Priority::High, 128)
        .unwrap();

    let first = scheduler.schedule_next().unwrap();
    assert_eq!(first, high);

    scheduler.force_preempt();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    scheduler.set_last_scheduled(low2, now.saturating_sub(800));

    let next = scheduler.schedule_next().unwrap();
    assert_eq!(next, low2, "low2 should be boosted above high due to aging");
}

#[test]
fn test_context_store_wired_to_scheduler() {
    let mut scheduler = Scheduler::new(8192);
    let mut ctx = EmbeddedContextStore::new(10_000);

    let pid1 = scheduler
        .spawn_process("worker_a", Priority::Normal, 128)
        .unwrap();
    let pid2 = scheduler
        .spawn_process("worker_b", Priority::Low, 256)
        .unwrap();

    ctx.telemetry_mut().record(
        aios_context::telemetry::TelemetryEntry::new("worker_a", 42.0, 100)
            .with_block(pid1.0 as u32),
    );
    ctx.telemetry_mut().record(
        aios_context::telemetry::TelemetryEntry::new("worker_b", 10.0, 100)
            .with_block(pid2.0 as u32),
    );

    let avg_a = ctx.telemetry().average_value("worker_a").unwrap();
    let avg_b = ctx.telemetry().average_value("worker_b").unwrap();
    assert!(avg_a > avg_b);

    let proc_a = scheduler.get_process(pid1).unwrap();
    let proc_b = scheduler.get_process(pid2).unwrap();
    assert_eq!(proc_a.priority, Priority::Normal);
    assert_eq!(proc_b.priority, Priority::Low);

    if avg_b < avg_a * 0.5 {
        let boosted = Priority::from_u8(proc_b.priority as u8 + 1);
        scheduler.set_priority(pid2, boosted).unwrap();
        let proc_b2 = scheduler.get_process(pid2).unwrap();
        assert_eq!(proc_b2.priority, Priority::Normal);
    }

    let _ = scheduler.schedule_next();
    assert!(scheduler.is_scheduled());
}

// ============================================================
// Test 21: IPC Bus backpressure + dedup + metrics
// ============================================================
#[test]
fn test_ipc_bus_backpressure_dedup_metrics() {
    let mut bus = IpcBus::new(5)
        .with_backpressure(BackpressurePolicy::DropOldest)
        .with_dedup();

    for i in 0..5u32 {
        bus.send(IpcPacket::new(0, i, CommandId::HealthCheck, Payload::Empty))
            .unwrap();
    }
    assert_eq!(bus.len(), 5);
    assert_eq!(bus.metrics().total_sent, 5);

    let mut dup = IpcPacket::new(0, 0, CommandId::HealthCheck, Payload::Empty);
    dup.header.packet_id = bus.peek().unwrap().header.packet_id;
    bus.send(dup).unwrap();
    assert_eq!(bus.len(), 5);
    assert_eq!(bus.metrics().total_deduplicated, 1);

    bus.send(IpcPacket::new(
        0,
        99,
        CommandId::HealthCheck,
        Payload::Empty,
    ))
    .unwrap();
    assert_eq!(bus.len(), 5);
    assert_eq!(bus.metrics().total_dropped, 1);
    assert_eq!(bus.metrics().peak_queue_depth, 5);

    let first = bus.receive().unwrap();
    assert_eq!(first.header.target_block, 1);

    bus.reset_metrics();
    assert_eq!(bus.metrics().total_sent, 0);
}

// ============================================================
// Test 22: Scheduler weighted round-robin within same priority
// ============================================================
#[test]
fn test_scheduler_weighted_round_robin() {
    let mut scheduler = Scheduler::new(8192);

    let _p1 = scheduler
        .spawn_process("normal_a", Priority::Normal, 64)
        .unwrap();
    let _p2 = scheduler
        .spawn_process("normal_b", Priority::Normal, 64)
        .unwrap();
    let _p3 = scheduler
        .spawn_process("normal_c", Priority::Normal, 64)
        .unwrap();

    let mut seen = Vec::new();
    for _ in 0..6 {
        if let Some(pid) = scheduler.schedule_next() {
            seen.push(pid);
            scheduler.force_preempt();
        }
    }
    assert_eq!(seen.len(), 6);
    assert_eq!(seen[0], seen[3]);
    assert_eq!(seen[1], seen[4]);
    assert_eq!(seen[2], seen[5]);
    assert_ne!(seen[0], seen[1]);
    assert_ne!(seen[1], seen[2]);
}

// ============================================================
// Test 23: Scheduler memory pressure detection
// ============================================================
#[test]
fn test_scheduler_memory_pressure_detection() {
    let mut scheduler = Scheduler::new(1024).with_memory_pressure_threshold(0.5);

    scheduler.register_memory_pressure_callback("gc_collect".into());
    scheduler
        .spawn_process("big1", Priority::Normal, 400)
        .unwrap();

    let event = scheduler.check_memory_pressure().unwrap();
    assert_eq!(
        event.level,
        aios_process_mgr::scheduler::PressureLevel::Warning
    );

    scheduler
        .spawn_process("big2", Priority::High, 200)
        .unwrap();
    let event = scheduler.check_memory_pressure().unwrap();
    assert_eq!(
        event.level,
        aios_process_mgr::scheduler::PressureLevel::Critical
    );
    assert_eq!(event.callbacks, vec!["gc_collect".to_string()]);
}

// ============================================================
// Test 24: Block dependency graph topological load order
// ============================================================
#[test]
fn test_block_dependency_graph_ordering() {
    let mut graph = DependencyGraph::new();
    graph.add_dependency("scheduler", "ipc_bus").unwrap();
    graph.add_dependency("scheduler", "hal").unwrap();
    graph.add_dependency("ipc_bus", "hal").unwrap();
    graph.add_dependency("orchestrator", "scheduler").unwrap();
    graph.add_dependency("orchestrator", "context").unwrap();
    graph.add_dependency("context", "telemetry").unwrap();

    let load = graph.load_order().unwrap();
    assert_eq!(load.len(), 6);

    let pos = |name: &str| load.iter().position(|x| x == name).unwrap();
    assert!(pos("hal") < pos("ipc_bus"));
    assert!(pos("ipc_bus") < pos("scheduler"));
    assert!(pos("scheduler") < pos("orchestrator"));
    assert!(pos("telemetry") < pos("context"));
    assert!(pos("context") < pos("orchestrator"));

    let unload = graph.unload_order().unwrap();
    assert_eq!(unload.len(), 6);
    let u = |name: &str| unload.iter().position(|x| x == name).unwrap();
    assert!(u("ipc_bus") < u("hal"));
    assert!(u("scheduler") < u("ipc_bus"));
    assert!(u("context") < u("telemetry"));
    assert!(u("orchestrator") < u("scheduler"));
    assert!(u("orchestrator") < u("context"));
}

// ============================================================
// Test 25: Semantic version integration with block registry
// ============================================================
#[test]
fn test_semantic_version_with_block_registry() {
    let mut registry = BlockRegistry::new();
    let v1 = SemanticVersion::parse("1.0.0").unwrap();
    let v2 = SemanticVersion::parse("1.1.0").unwrap();
    let v3 = SemanticVersion::parse("2.0.0").unwrap();

    assert!(v2.is_compatible_with(&v1));
    assert!(!v3.is_compatible_with(&v1));
    assert!(v2.is_newer_than(&v1));

    let _id1 = registry
        .register_block("module_v1", &format!("{}", v1), vec![1, 2, 3])
        .unwrap();
    let _id2 = registry
        .register_block("module_v2", &format!("{}", v2), vec![4, 5, 6])
        .unwrap();

    assert_eq!(registry.count(), 2);
    assert!(registry.find_by_name("module_v1").is_some());
    assert!(registry.find_by_name("module_v2").is_some());

    let mut v = v1.clone();
    v.bump_major();
    assert_eq!(v, v3);
}

// ============================================================
// Test 26: IPC bus priority queue ordering
// ============================================================
#[test]
fn test_ipc_bus_priority_cross_crates() {
    let mut bus = IpcBus::new(100);

    let low = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::Empty).with_priority(1);
    let high = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::Empty).with_priority(10);
    let normal = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::Empty).with_priority(5);

    bus.send_priority(low).unwrap();
    bus.send_priority(normal).unwrap();
    bus.send_priority(high).unwrap();

    let first = bus.receive().unwrap();
    assert_eq!(first.header.priority, 10);
    let second = bus.receive().unwrap();
    assert_eq!(second.header.priority, 5);
    let third = bus.receive().unwrap();
    assert_eq!(third.header.priority, 1);
}

// ============================================================
// Test 27: Dependency graph cycle detection
// ============================================================
#[test]
fn test_dependency_graph_complex_cycle() {
    let mut graph = DependencyGraph::new();
    graph.add_dependency("a", "b").unwrap();
    graph.add_dependency("b", "c").unwrap();

    let result = graph.add_dependency("c", "a");
    assert!(result.is_err());

    let load_result = graph.load_order();
    assert!(load_result.is_ok());
    let load = load_result.unwrap();
    let pos_a = load.iter().position(|x| x == "a").unwrap();
    let pos_b = load.iter().position(|x| x == "b").unwrap();
    let pos_c = load.iter().position(|x| x == "c").unwrap();
    assert!(pos_c < pos_b);
    assert!(pos_b < pos_a);

    graph.remove_block("c");
    assert!(!graph.has_block("c"));
    let load2 = graph.load_order().unwrap();
    assert_eq!(load2.len(), 2);
}

// ============================================================
// Test 28: Cross-subsystem integration — scheduler + security + IPC
// ============================================================
#[test]
fn test_cross_subsystem_scheduler_security_ipc() {
    let mut scheduler = Scheduler::new(8192);
    let mut acl = AccessControlLayer::new(b"cross_key".to_vec(), 60_000);
    let mut bus = IpcBus::new(100).with_backpressure(BackpressurePolicy::Reject);

    acl.issue_token(1, vec![Capability::ProcessSpawn, Capability::BlockLoad])
        .unwrap();
    acl.issue_token(2, vec![Capability::FsRead]).unwrap();

    let pid = scheduler
        .spawn_process("trusted_worker", Priority::High, 256)
        .unwrap();
    assert!(acl.check_permission(1, &Capability::ProcessSpawn).is_ok());
    assert!(acl.check_permission(2, &Capability::ProcessSpawn).is_err());

    let pkt = IpcPacket::new(1, 0, CommandId::HealthCheck, Payload::HealthCheck).with_priority(8);
    bus.send_priority(pkt).unwrap();
    assert_eq!(bus.peek().unwrap().header.priority, 8);

    let next = scheduler.schedule_next().unwrap();
    assert_eq!(next, pid);
}

// ============================================================
// Test 29: Block store update flow (local source + publish + rollback)
// ============================================================
#[test]
fn test_block_store_update_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("source");
    let blocks_dir = tmp.path().join("installed");

    let v1 = b"block-v1-binary";
    let v2 = b"block-v2-binary";
    let source_blocks = source_dir.join("blocks");
    std::fs::create_dir_all(&source_blocks).unwrap();
    std::fs::write(source_blocks.join("net_1.0.0.wasm"), v1).unwrap();

    let mut manager = aios_store::manager::StoreManager::with_sources(
        vec![aios_store::source::StoreSource::local(
            &source_dir.to_string_lossy(),
        )],
        &blocks_dir,
    );

    let found = aios_store::manager::StoreManager::block_on(manager.search("net", None)).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "net");
    assert_eq!(found[0].version, "1.0.0");

    let installed =
        aios_store::manager::StoreManager::block_on(manager.install(None, "net", None)).unwrap();
    assert_eq!(installed.manifest.version, "1.0.0");
    assert_eq!(std::fs::read(&installed.path).unwrap(), v1);

    let bad_manifest = aios_store::manifest::ManifestInfo {
        name: "net".into(),
        version: "9.9.9".into(),
        description: "tampered".into(),
        author: "attacker".into(),
        capabilities: std::collections::HashSet::new(),
        wasm_size_bytes: v2.len() as u64,
        wasm_sha256: "0".repeat(64),
        signature: None,
        store_url: None,
    };
    assert!(manager
        .installer
        .install_from_bytes(bad_manifest, v2)
        .is_err());

    std::fs::write(source_blocks.join("net_2.0.0.wasm"), v2).unwrap();

    let updated =
        aios_store::manager::StoreManager::block_on(manager.update(None, Some("net"))).unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].manifest.version, "2.0.0");

    let rolled_back = manager.rollback("net").unwrap();
    assert_eq!(rolled_back.manifest.version, "1.0.0");
    assert_eq!(manager.list_installed().len(), 1);
}

// ============================================================
// Test 30: Network settings block roundtrip (net get/set/reset)
// ============================================================
#[test]
fn test_net_settings_block_roundtrip() {
    use aios_net_config::block::NetSettingsBlock;
    use aios_net_config::config::NetworkConfig;

    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("net.json");

    let mut block = NetSettingsBlock::new(BlockId::new(9), NetworkConfig::default(), &store_path);
    assert_eq!(block.config().hostname, "aios-host");

    block
        .apply(&serde_json::json!({ "hostname": "e2e-host", "listen_port": 9090 }))
        .unwrap();
    assert_eq!(block.config().hostname, "e2e-host");
    assert_eq!(block.config().listen_port, 9090);

    let reloaded = NetSettingsBlock::new(BlockId::new(9), NetworkConfig::default(), &store_path);
    assert_eq!(reloaded.config().hostname, "e2e-host");
    assert_eq!(reloaded.config().listen_port, 9090);

    block.reset().unwrap();
    assert_eq!(block.config().hostname, "aios-host");
    assert_eq!(block.config().listen_port, 8080);
}
