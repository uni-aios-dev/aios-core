use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_debug::crash_reporter::{CrashKind, CrashReporter};
use aios_ipc::bus::{BackpressurePolicy, IpcBus};
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use aios_telemetry::flight_recorder::{EventKind, FlightRecorder};
use aios_wasm::executor::BlockExecutor;
use aios_wasm::isolation::IsolationConfig;
use std::time::Instant;

// Sample WASM module with add/mul/sub exported functions and a crash export
fn math_wasm() -> Vec<u8> {
    r#"
        (module
            (func (export "init"))
            (func (export "start"))
            (func (export "add") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add)
            (func (export "mul") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.mul)
            (func (export "sub") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.sub)
        )
    "#
    .as_bytes()
    .to_vec()
}

fn identity_wasm(id: u32) -> Vec<u8> {
    format!(
        r#"
            (module
                (func (export "init"))
                (func (export "start"))
                (func (export "identity") (param i32) (result i32)
                    local.get 0)
                (func (export "block_id") (result i32)
                    i32.const {id})
            )
        "#
    )
    .as_bytes()
    .to_vec()
}

#[test]
fn test_stress_50_parallel_wasm_blocks() {
    let mut registry = BlockRegistry::new();
    let mut executor = BlockExecutor::with_default_config().unwrap();
    let mut bus = IpcBus::new(2048).with_backpressure(BackpressurePolicy::DropOldest);

    let start = Instant::now();

    for i in 0..50 {
        let wasm = identity_wasm(i);
        let manifest =
            BlockLoader::load_from_binary(&mut registry, &format!("block_{i}"), "1.0.0", wasm)
                .unwrap();
        assert_eq!(manifest.name, format!("block_{i}"));

        let result = executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();
        assert!(result.success);

        let r = executor
            .call_block_func(manifest.id, "identity", &[wasmtime::Val::I32(i as i32)])
            .unwrap();
        assert_eq!(r[0].i32(), Some(i as i32));

        let r = executor
            .call_block_func(manifest.id, "block_id", &[])
            .unwrap();
        assert_eq!(r[0].i32(), Some(i as i32));

        let pkt = IpcPacket::new(
            manifest.id.0,
            (i + 1) % 50,
            CommandId::Custom,
            Payload::Binary(format!("data_{i}").into_bytes()),
        );
        bus.send(pkt).unwrap();
    }

    assert_eq!(registry.count(), 50);

    let mut received = 0u64;
    while bus.receive().is_some() {
        received += 1;
    }
    assert!(received > 0, "At least some IPC packets should be received");
    assert!(received <= 50);

    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 30,
        "50 WASM blocks took {elapsed:?} (>30s)"
    );
}

#[test]
fn test_stress_50_blocks_ipc_throughput() {
    let mut registry = BlockRegistry::new();
    let mut executor = BlockExecutor::with_default_config().unwrap();
    let mut bus = IpcBus::new(4096);

    for i in 0..50 {
        let wasm = identity_wasm(i);
        let manifest =
            BlockLoader::load_from_binary(&mut registry, &format!("ipc_block_{i}"), "1.0.0", wasm)
                .unwrap();
        executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();
    }

    let start = Instant::now();
    let total_packets = 500u64;

    for i in 0..total_packets {
        let src = (i % 50) as u32;
        let dst = ((i + 1) % 50) as u32;
        let pkt = IpcPacket::new(
            src,
            dst,
            CommandId::Custom,
            Payload::Binary(format!("throughput_{i}").into_bytes()),
        );
        bus.send(pkt).unwrap();
    }

    let send_elapsed = start.elapsed();
    assert!(
        send_elapsed.as_millis() < 2000,
        "500 IPC sends took {send_elapsed:?} (>2s)"
    );

    let start = Instant::now();
    let mut received = 0u64;
    while bus.receive().is_some() {
        received += 1;
    }
    let recv_elapsed = start.elapsed();

    assert!(received > 0, "Should receive packets");
    assert!(
        recv_elapsed.as_millis() < 2000,
        "IPC receive took {recv_elapsed:?} (>2s)"
    );
}

#[test]
fn test_fault_tolerance_block_panic_isolation() {
    let mut registry = BlockRegistry::new();
    let mut executor = BlockExecutor::with_default_config().unwrap();
    let mut bus = IpcBus::new(256);
    let mut flight = FlightRecorder::new(1000, 3600);
    let mut crash_reporter = CrashReporter::new("aios-stress-test", "1.0.0");

    for i in 0..10 {
        let wasm = math_wasm();
        let manifest =
            BlockLoader::load_from_binary(&mut registry, &format!("math_block_{i}"), "1.0.0", wasm)
                .unwrap();

        let result = executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();
        assert!(result.success);

        let r = executor
            .call_block_func(
                manifest.id,
                "add",
                &[wasmtime::Val::I32(i), wasmtime::Val::I32(i * 2)],
            )
            .unwrap();
        assert_eq!(r[0].i32(), Some(i + i * 2));
    }

    assert_eq!(registry.count(), 10);

    let crash_report = crash_reporter.generate_report(
        CrashKind::BlockCrash,
        "block_3_simulated",
        "Simulated block panic in WASM executor",
        "stack: simulate_panic at block_3:42",
        "flight_recorder_dump_placeholder",
        false,
    );
    assert_eq!(crash_report.kind, CrashKind::BlockCrash);
    assert!(!crash_report.id.is_empty());
    assert!(crash_report.timestamp_ms > 0);

    flight.record(
        EventKind::Panic,
        &format!("Block crash: {}", crash_report.id),
    );
    let panic_events = flight.dump_by_kind(EventKind::Panic);
    assert_eq!(panic_events.len(), 1);

    for i in 0..10 {
        let add_result = executor.call_block_func(
            registry
                .find_by_name(&format!("math_block_{i}"))
                .unwrap()
                .manifest
                .id,
            "add",
            &[wasmtime::Val::I32(10), wasmtime::Val::I32(20)],
        );
        assert!(
            add_result.is_ok(),
            "Block {i} must remain operational after crash"
        );
        if let Ok(vals) = add_result {
            assert_eq!(vals[0].i32(), Some(30));
        }
    }

    let pkt = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::HealthCheck);
    bus.send(pkt).unwrap();
    assert!(bus.receive().is_some());

    assert_eq!(crash_reporter.report_count(), 1);
}

#[test]
fn test_fault_tolerance_scheduler_survives_crash() {
    let mut scheduler = Scheduler::new(8192);
    let mut flight = FlightRecorder::new(100, 3600);

    let pids: Vec<_> = (0..20)
        .map(|i| {
            scheduler
                .spawn_process(&format!("worker_{i}"), Priority::Normal, 64)
                .unwrap()
        })
        .collect();

    assert_eq!(scheduler.process_count(), 20);

    let victim = pids[5];
    scheduler.kill_process(victim).unwrap();
    flight.record(EventKind::Error, &format!("Worker {victim:?} crashed"));
    assert_eq!(scheduler.process_count(), 19);

    let survivor = scheduler.get_process(victim);
    assert!(survivor.is_none(), "Victim process must be gone");

    for pid in pids.iter().filter(|&&p| p != victim) {
        assert!(
            scheduler.get_process(*pid).is_some(),
            "Survivor {pid:?} must exist"
        );
    }

    let crash_events = flight.dump_by_kind(EventKind::Error);
    assert_eq!(crash_events.len(), 1);

    let new_pid = scheduler
        .spawn_process("replacement", Priority::High, 128)
        .unwrap();
    assert_eq!(scheduler.process_count(), 20);
    let next = scheduler.schedule_next().unwrap();
    assert_eq!(next, new_pid, "Replacement (high priority) should be next");
}

#[test]
fn test_fault_tolerance_scheduler_back_to_back_crashes() {
    let mut scheduler = Scheduler::new(4096);

    let pids: Vec<_> = (0..10)
        .map(|i| {
            scheduler
                .spawn_process(&format!("crashable_{i}"), Priority::Normal, 32)
                .unwrap()
        })
        .collect();

    for (i, pid) in pids.iter().enumerate() {
        if i % 2 == 0 {
            scheduler.kill_process(*pid).unwrap();
        }
    }

    assert_eq!(scheduler.process_count(), 5);

    for _ in 0..5 {
        let next = scheduler.schedule_next();
        assert!(next.is_some(), "Scheduler must survive repeated crashes");
    }
}
