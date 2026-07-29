use aios_block_mgr::registry::BlockRegistry;
use aios_block_mgr::router::MessageRouter;
use aios_context::persistence::PersistentStore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::TelemetryEntry;
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_hal::hardware::{HardwareProfile, StorageInterface};
use aios_ipc::bus::{BackpressurePolicy, IpcBus};
use aios_live_update::engine::LiveUpdateEngine;
use aios_process_mgr::scheduler::{Scheduler, SchedulingMode};
use aios_process_mgr::task::Priority;
use aios_watchdog::heartbeat::Heartbeat;
use std::time::Instant;

// ============================================================
// Stress Test 1: Mass process spawn/schedule
// ============================================================
#[test]
fn test_stress_mass_spawn_1000() {
    let mut scheduler = Scheduler::new(64 * 1024);
    let start = Instant::now();
    for i in 0..1000 {
        let name = format!("proc_{}", i);
        scheduler.spawn_process(&name, Priority::Normal, 4).unwrap();
    }
    let elapsed = start.elapsed();

    let start = Instant::now();
    let mut scheduled = 0u64;
    while scheduler.schedule_next().is_some() {
        scheduler.tick();
        scheduled += 1;
        if scheduled > 5000 {
            break;
        }
    }
    let elapsed_sched = start.elapsed();
    assert!(
        elapsed.as_millis() < 500,
        "Spawn 1000 took {:?} (>500ms)",
        elapsed
    );
    assert!(
        elapsed_sched.as_millis() < 2000,
        "Schedule loop took {:?} (>2s)",
        elapsed_sched
    );
}

// ============================================================
// Stress Test 2: IPC bus throughput
// ============================================================
#[test]
fn test_stress_ipc_bus_throughput() {
    let mut bus = IpcBus::new(1024).with_backpressure(BackpressurePolicy::DropOldest);
    let count = 10_000u64;

    let start = Instant::now();
    for i in 0..count {
        let pkt = IpcPacket::new(0, (i % 100) as u32, CommandId::HealthCheck, Payload::Empty);
        let _ = bus.send(pkt);
    }
    let send_elapsed = start.elapsed();

    let start = Instant::now();
    let mut received = 0u64;
    while bus.receive().is_some() {
        received += 1;
    }
    let recv_elapsed = start.elapsed();
    assert!(received > 0, "Should receive at least some packets");
    assert!(
        send_elapsed.as_millis() < 1000,
        "IPC send took {:?} (>1s)",
        send_elapsed
    );
    assert!(
        recv_elapsed.as_millis() < 1000,
        "IPC recv took {:?} (>1s)",
        recv_elapsed
    );
}

// ============================================================
// Stress Test 3: RT scheduler with many deadline tasks
// ============================================================
#[test]
fn test_stress_rt_scheduler_500() {
    let mut scheduler = Scheduler::new(64 * 1024);
    scheduler.set_scheduling_mode(SchedulingMode::RealTime);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    for i in 0..500 {
        let name = format!("rt_{}", i);
        let pid = scheduler.spawn_process(&name, Priority::High, 4).unwrap();
        scheduler.set_rt_deadline(pid, now + (500 - i) as u64);
    }

    let start = Instant::now();
    let mut count = 0u64;
    while let Some(_pid) = scheduler.schedule_next() {
        count += 1;
        scheduler.tick();
        if count > 2000 {
            break;
        }
    }
    let elapsed = start.elapsed();
    assert!(count > 500, "Scheduled {} processes (<500)", count);
    assert!(
        elapsed.as_millis() < 2000,
        "RT scheduling took {:?} (>2s)",
        elapsed
    );
}

// ============================================================
// Stress Test 4: Block registry mass register/load
// ============================================================
#[test]
fn test_stress_block_registry_500() {
    let mut registry = BlockRegistry::new();
    let start = Instant::now();
    for i in 0..500 {
        let name = format!("block_{}", i);
        registry
            .register_block(&name, "1.0.0", vec![i as u8; 64])
            .unwrap();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 500,
        "Register 500 blocks took {:?} (>500ms)",
        elapsed
    );

    assert_eq!(registry.count(), 500);

    let topo = registry.topology();
    assert_eq!(topo.len(), 500);

    for i in 0..500 {
        let name = format!("block_{}", i);
        assert!(registry.find_by_name(&name).is_some());
    }
}

// ============================================================
// Stress Test 5: Context store + telemetry
// ============================================================
#[test]
fn test_stress_context_store_1000() {
    let mut store = EmbeddedContextStore::new(2000);
    let start = Instant::now();
    for i in 0..1000 {
        let entry = TelemetryEntry::new(&format!("metric_{}", i % 10), i as f64, (i * 4) as u64);
        store.telemetry_mut().record(entry);
    }
    let elapsed = start.elapsed();
    assert_eq!(store.telemetry().count(), 1000);
    assert!(
        elapsed.as_millis() < 500,
        "1000 telemetry entries took {:?} (>500ms)",
        elapsed
    );

    assert!(store.telemetry().average_value("metric_0").is_some());
    assert!(store.telemetry().peak_ram() > 0);
}

// ============================================================
// Stress Test 6: Hardware mock serialization
// ============================================================
#[test]
fn test_stress_hardware_mock_serialize() {
    let profile = HardwareProfile::mock_modern();
    let start = Instant::now();
    for _ in 0..10_000 {
        let bytes = bincode::serialize(&profile).unwrap();
        let _: HardwareProfile = bincode::deserialize(&bytes).unwrap();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 2000,
        "10k HW serialize/deserialize took {:?} (>2s)",
        elapsed
    );
}

// ============================================================
// Stress Test 7: Heartbeat batch verification
// ============================================================
#[test]
fn test_stress_heartbeat_1000() {
    let secret = b"stress_test_secret_key_32bytes!";
    let start = Instant::now();
    for i in 0..1000 {
        let hb = Heartbeat::new(i, secret);
        assert!(hb.verify(secret));
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 1000,
        "1000 heartbeat cycles took {:?} (>1s)",
        elapsed
    );
}

// ============================================================
// Stress Test 8: Storage detection NVMe + SATA
// ============================================================
#[test]
fn test_stress_storage_profiles() {
    let modern = HardwareProfile::mock_modern();
    assert_eq!(modern.storage_devices.len(), 2);
    assert_eq!(modern.storage_devices[0].interface, StorageInterface::NVMe);
    assert!(modern.storage_devices[0].capacity_gb > 0);

    let legacy = HardwareProfile::mock_legacy();
    assert_eq!(legacy.storage_devices.len(), 1);
    assert_eq!(legacy.storage_devices[0].interface, StorageInterface::SATA);

    let nvidia = HardwareProfile::mock_nvidia();
    assert_eq!(nvidia.storage_devices.len(), 1);
    assert_eq!(nvidia.storage_devices[0].interface, StorageInterface::NVMe);
}

// ============================================================
// Stress Test 9: Message router dispatch
// ============================================================
#[test]
fn test_stress_message_router_500() {
    let mut router = MessageRouter::new();

    for i in 0..500u32 {
        let block_id = i + 1;
        router.register_handler(
            block_id,
            Box::new(move |pkt: &IpcPacket| {
                Ok(Some(IpcPacket::response_ok(
                    pkt.header.target_block,
                    pkt.header.source_block,
                    pkt.header.packet_id,
                    Payload::Binary(vec![block_id as u8; 4]),
                )))
            }),
        );
    }

    let start = Instant::now();
    for i in 0..500u32 {
        let target = i + 1;
        let pkt = IpcPacket::new(0, target, CommandId::HealthCheck, Payload::Empty);
        let resp = router.dispatch(&pkt).unwrap().unwrap();
        assert!(resp.verify_checksum());
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 500,
        "Router 500 dispatches took {:?} (>500ms)",
        elapsed
    );
}

// ============================================================
// Stress Test 10: Live update engine batch swaps
// ============================================================
#[test]
fn test_stress_live_update_20() {
    let mut engine = LiveUpdateEngine::new(5000);

    for i in 0..20u32 {
        let mut bus = IpcBus::new(10);
        bus.send(IpcPacket::new(0, i, CommandId::HealthCheck, Payload::Empty))
            .unwrap();

        let old_bin = format!("old_binary_{}", i).into_bytes();
        let new_bin = format!("new_binary_{}", i).into_bytes();
        let hash = aios_core::crypto::compute_sha256_bytes(&new_bin);

        engine
            .perform_swap(
                i,
                old_bin,
                "0.1.0".into(),
                vec![],
                new_bin,
                "0.2.0".into(),
                hash,
                &mut bus,
                None,
            )
            .unwrap();

        assert!(engine.has_rollback(i));
    }

    assert_eq!(engine.pending_rollbacks().len(), 20);
}

// ============================================================
// Stress Test 11: PersistentStore batch write
// ============================================================
#[test]
fn test_stress_persistent_store_batch() {
    let dir = std::env::temp_dir().join("aios_stress_persist");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stress.redb");
    let store = PersistentStore::new(&path);

    let entries: Vec<TelemetryEntry> = (0..500)
        .map(|i| TelemetryEntry::new(&format!("metric_{}", i), i as f64, (i * 4) as u64))
        .collect();

    let start = Instant::now();
    let count = store.save_telemetry(&entries).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(count, 500);

    let loaded = store.load_telemetry().unwrap();
    assert_eq!(loaded.len(), 500);

    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        elapsed.as_millis() < 3000,
        "500 persist writes took {:?} (>3s)",
        elapsed
    );
}
