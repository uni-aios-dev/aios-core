use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::TelemetryEntry;
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_debug::crash_reporter::CrashKind;
use aios_ipc::bus::{BackpressurePolicy, IpcBus};
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use aios_security::access_control::AccessControlLayer;
use aios_security::capability::Capability;
use aios_watchdog::heartbeat::Heartbeat;
use aios_watchdog::safe_mode::SafeModeShell;
use aios_watchdog::watchdog::{Watchdog, WatchdogAction, WatchdogConfig, WatchdogState};

// ============================================================
// Chaos 1: IPC packet corruption — malformed data must not crash
// ============================================================
#[test]
fn test_chaos_corrupted_ipc_packet() {
    let mut bus = IpcBus::new(50);

    let valid = IpcPacket::new(1, 0, CommandId::HealthCheck, Payload::HealthCheck);
    bus.send(valid).unwrap();

    let corrupted = IpcPacket::new(999, 999, CommandId::HealthCheck, Payload::HealthCheck);
    bus.send(corrupted).unwrap();

    let p1 = bus.receive().unwrap();
    assert_eq!(p1.header.source_block, 1);

    let p2 = bus.receive().unwrap();
    assert_eq!(p2.header.source_block, 999);
}

// ============================================================
// Chaos 2: Bus overflow — fill to capacity, backpressure works
// ============================================================
#[test]
fn test_chaos_bus_overflow() {
    let mut bus = IpcBus::new(10).with_backpressure(BackpressurePolicy::Reject);

    for i in 0..10 {
        let pkt = IpcPacket::new(i as u32, 0, CommandId::HealthCheck, Payload::HealthCheck);
        bus.send_priority(pkt).unwrap();
    }

    let extra = IpcPacket::new(99, 0, CommandId::HealthCheck, Payload::HealthCheck);
    let result = bus.send_priority(extra);
    assert!(result.is_err());

    for _ in 0..10 {
        assert!(bus.receive().is_some());
    }
    assert!(bus.receive().is_none());
}

// ============================================================
// Chaos 3: Scheduler memory exhaustion
// ============================================================
#[test]
fn test_chaos_scheduler_memory_exhaustion() {
    let mut scheduler = Scheduler::new(128);

    let mut spawned = 0u64;
    while scheduler.spawn_process("filler", Priority::Low, 8).is_ok() {
        spawned += 1;
        if spawned > 500 {
            break;
        }
    }

    let count = scheduler.process_count();
    assert!(count > 0);

    let scheduled = scheduler.schedule_next();
    assert!(scheduled.is_some());
}

// ============================================================
// Chaos 4: Scheduler crash-resilience — kill + restart loops
// ============================================================
#[test]
fn test_chaos_scheduler_crash_loop() {
    let mut scheduler = Scheduler::new(8192);

    let mut current_pid = scheduler
        .spawn_process("crasher", Priority::High, 256)
        .unwrap();

    for _ in 0..5 {
        scheduler.kill_process(current_pid).unwrap();
        assert_eq!(scheduler.process_count(), 0);

        current_pid = scheduler
            .spawn_process("crasher_v2", Priority::High, 256)
            .unwrap();
    }

    assert_eq!(scheduler.process_count(), 1);
}

// ============================================================
// Chaos 5: Watchdog timeout → safe mode transition
// ============================================================
#[test]
fn test_chaos_watchdog_timeout() {
    let config = WatchdogConfig {
        heartbeat_interval_ms: 10,
        max_missed_heartbeats: 1,
        warn_threshold: 1,
        recovery_timeout_ms: 50,
        secret: b"chaos_secret".to_vec(),
    };
    let mut watchdog = Watchdog::new(config);

    assert_eq!(watchdog.state(), WatchdogState::Monitoring);

    let action = watchdog.check_timeout();
    assert!(matches!(
        action,
        WatchdogAction::SuspendOrchestrator | WatchdogAction::AttemptRecovery
    ));

    watchdog.force_safe_mode();
    assert_eq!(watchdog.state(), WatchdogState::SafeMode);
}

// ============================================================
// Chaos 6: Safe mode shell under rapid commands
// ============================================================
#[test]
fn test_chaos_safe_mode_rapid_commands() {
    let mut shell = SafeModeShell::new(3);
    let mut registry = BlockRegistry::new();
    let mut scheduler = Scheduler::new(8192);

    let commands = vec![
        SafeModeShell::parse_command("spawn worker1 5 128"),
        SafeModeShell::parse_command("ps"),
        SafeModeShell::parse_command("status"),
        SafeModeShell::parse_command("help"),
        SafeModeShell::parse_command("spawn worker2 3 64"),
        SafeModeShell::parse_command("blocks"),
        SafeModeShell::parse_command("ps"),
    ];

    for cmd in &commands {
        let _ = shell.execute(cmd.clone(), &mut scheduler, &mut registry);
    }

    assert!(!shell.log_entries().is_empty());
}

// ============================================================
// Chaos 7: ACL — missing token rejected, wrong cap rejected
// ============================================================
#[test]
fn test_chaos_acl_token_corruption() {
    let mut acl = AccessControlLayer::new(b"real_secret".to_vec(), 60_000);

    acl.issue_token(1, vec![Capability::FsRead]).unwrap();

    assert!(acl.check_permission(1, &Capability::FsRead).is_ok());

    assert!(acl.check_permission(2, &Capability::FsRead).is_err());

    assert!(acl.check_permission(1, &Capability::ProcessSpawn).is_err());
}

// ============================================================
// Chaos 8: Heartbeat forgery — HMAC verification
// ============================================================
#[test]
fn test_chaos_heartbeat_forgery() {
    let secret = b"watchdog_secret";
    let real = Heartbeat::new(1, secret);
    assert!(real.verify(secret));

    let forged = Heartbeat::new(2, b"other_secret");
    assert!(!forged.verify(secret));

    let mut tampered = Heartbeat::new(3, secret);
    tampered.sequence = 999;
    assert!(!tampered.verify(secret));
}

// ============================================================
// Chaos 9: Context store compaction under load
// ============================================================
#[test]
fn test_chaos_context_store_exhaustion() {
    let mut store = EmbeddedContextStore::with_compact_threshold(200, 0.0);

    for i in 0..1000 {
        store
            .telemetry_mut()
            .record(TelemetryEntry::new("metric", i as f64, 256));
    }

    assert!(store.telemetry().entries.len() <= 10_000);

    store
        .telemetry_mut()
        .record(TelemetryEntry::new("overflow", 42.0, 128));
    let before = store.total_entries();

    let report = store.compact();
    assert!(report.telemetry_pruned > 0);
    assert!(store.total_entries() < before);
}

// ============================================================
// Chaos 10: Block loader duplicate registration
// ============================================================
#[test]
fn test_chaos_block_loader_duplicate() {
    let mut registry = BlockRegistry::new();

    let r1 = BlockLoader::load_from_binary(&mut registry, "chaos_block", "1.0.0", b"v1".to_vec());
    assert!(r1.is_ok());

    let r2 = BlockLoader::load_from_binary(&mut registry, "chaos_block", "1.0.0", b"v1".to_vec());
    assert!(r2.is_ok());
}

// ============================================================
// Chaos 11: Concurrent bus drain + send
// ============================================================
#[test]
fn test_chaos_bus_drain_under_load() {
    let mut bus = IpcBus::new(100);

    for i in 0..50 {
        let pkt = IpcPacket::new(i, 0, CommandId::HealthCheck, Payload::HealthCheck);
        bus.send(pkt).unwrap();
    }

    let mut received = 0;
    while bus.receive().is_some() {
        received += 1;

        if received == 25 {
            for j in 50..60 {
                let pkt = IpcPacket::new(j, 0, CommandId::HealthCheck, Payload::HealthCheck);
                let _ = bus.send_priority(pkt);
            }
        }
    }

    assert!(received >= 50);
}

// ============================================================
// Chaos 12: Watchdog HMAC forgery → rejection
// ============================================================
#[test]
fn test_chaos_watchdog_rejects_bad_heartbeat() {
    let config = WatchdogConfig {
        heartbeat_interval_ms: 1000,
        max_missed_heartbeats: 3,
        warn_threshold: 2,
        recovery_timeout_ms: 5000,
        secret: b"correct_secret".to_vec(),
    };
    let mut watchdog = Watchdog::new(config);

    let bad_heartbeat = Heartbeat::new(1, b"wrong_secret");
    let result = watchdog.receive_heartbeat(&bad_heartbeat);
    assert!(result.is_err());

    let good_heartbeat = Heartbeat::new(1, b"correct_secret");
    assert!(watchdog.receive_heartbeat(&good_heartbeat).is_ok());
}

// ============================================================
// Chaos 14: Rollback crash — simulate crash mid-snapshot with recovery
// ============================================================
#[test]
fn test_chaos_rollback_crash_mid_snapshot() {
    use aios_updater::rollback::RollbackManager;

    let mut mgr = RollbackManager::new(10);

    mgr.take_snapshot("safe", "1.0", vec![1, 2, 3], "pre-crash");
    mgr.take_snapshot("crashed", "2.0", vec![255; 1024], "during-crash");

    let recovered = mgr.rollback_last().unwrap();
    assert_eq!(recovered, vec![255; 1024]);
    assert_eq!(mgr.snapshot_count(), 1);
}

// ============================================================
// Chaos 15: Multi-thread crash — simulate simultaneous crashes
// ============================================================
#[test]
fn test_chaos_multi_thread_crash_reports() {
    use aios_debug::crash_reporter::CrashReporter;

    let mut cr = CrashReporter::new("aios-core", "1.0.0");

    for i in 0..10 {
        let kind = match i % 4 {
            0 => CrashKind::Panic,
            1 => CrashKind::OOM,
            2 => CrashKind::WatchdogTimeout,
            _ => CrashKind::BlockCrash,
        };
        let msg = format!("crash #{i} from thread");
        let stack = format!("stack:crash_{i}");
        cr.generate_report(kind, "worker", &msg, &stack, "fr", false);
    }

    assert_eq!(cr.report_count(), 10);
    assert!(cr.latest_report().is_some());
}

// ============================================================
// Chaos 16: Reporter under rapid fire — many reports in sequence
// ============================================================
#[test]
fn test_chaos_reporter_rapid_fire() {
    use aios_debug::crash_reporter::CrashReporter;

    let mut cr = CrashReporter::new("aios-core", "1.0.0");

    for i in 0..100 {
        cr.generate_report(
            CrashKind::Unknown,
            "load-generator",
            &format!("event #{i}"),
            "stack",
            "flight",
            i % 2 == 0,
        );
    }

    assert_eq!(cr.report_count(), 100);
    let json = cr.to_json();
    assert!(json.len() > 100);
    assert!(!json.contains("event #0"));
    assert!(json.contains("event #1"));
    assert!(json.contains("event #99"));
    assert!(json.contains("\"redacted\":true"));
}

// ============================================================
// Chaos 17: Rollback + data corruption recovery
// ============================================================
#[test]
fn test_chaos_rollback_corrupted_data_recovery() {
    use aios_updater::rollback::RollbackManager;

    let mut mgr = RollbackManager::new(5);

    mgr.take_snapshot("b", "1.0", vec![1, 2, 3], "good");
    mgr.take_snapshot("b", "2.0", vec![4, 5, 6], "corrupted");

    let recovered = mgr.rollback_to(1).unwrap();
    assert_eq!(recovered, vec![1, 2, 3]);
    assert_eq!(mgr.snapshot_count(), 0);
}

// ============================================================
// Chaos 18: Sequential crash + rollback + re-snapshot loop
// ============================================================
#[test]
fn test_chaos_rollback_crash_loop() {
    use aios_updater::rollback::RollbackManager;

    let mut mgr = RollbackManager::new(3);

    for i in 0..10 {
        let _ = mgr.take_snapshot("b", &format!("{i}.0"), vec![i], &format!("iter-{i}"));
        if i > 0 && i % 2 == 0 {
            let _ = mgr.rollback_last();
        }
    }

    assert!(mgr.snapshot_count() > 0);
    assert!(mgr.snapshot_count() <= 3);
}

// ============================================================
// Chaos 13: Process kill after schedule — scheduler consistency
// ============================================================
#[test]
fn test_chaos_kill_scheduled_process() {
    let mut scheduler = Scheduler::new(8192);

    let pid1 = scheduler
        .spawn_process("worker_a", Priority::High, 256)
        .unwrap();
    let pid2 = scheduler
        .spawn_process("worker_b", Priority::Normal, 128)
        .unwrap();

    let scheduled = scheduler.schedule_next().unwrap();
    assert_eq!(scheduled, pid1);

    scheduler.kill_process(pid1).unwrap();

    let next = scheduler.schedule_next().unwrap();
    assert_eq!(next, pid2);
}
