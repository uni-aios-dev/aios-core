use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_bridge::intent_engine::{
    BlockAction, IntentParser, MetricType, ProcessAction, UserIntent,
};
use aios_bridge::server::{start_server, BridgeContext};
use aios_builder::compiler::WorkflowCompiler;
use aios_builder::easylang::EasyLangParser;
use aios_context::persistence::PersistentStore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::TelemetryEntry;
use aios_core::crypto;
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_ipc::bus::IpcBus;
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use aios_ringbuf::{RingBuffer, RingBufferConfig};
use aios_security::access_control::AccessControlLayer;
use aios_telemetry::flight_recorder::{EventKind, FlightRecorder};
use aios_telemetry::MetricCollector;
use aios_watchdog::watchdog::{Watchdog, WatchdogConfig};
use std::sync::Arc;
use std::time::Duration;

const SECRET: &[u8] = b"e2e_test_secret_key_32bytes!";

#[test]
fn test_e2e_hw_core_profile() {
    let profile = HardwareProfile::mock_modern();
    assert!(!profile.cpu.model.is_empty(), "CPU model must be set");
    assert!(profile.cpu.cores > 0, "CPU cores must be > 0");
    assert!(profile.memory.total_mb > 0, "RAM total must be > 0");

    let ai_tier = AiTier::from_profile(&profile);
    assert_eq!(ai_tier, AiTier::Tier1);

    let json = serde_json::to_string(&profile).unwrap();
    assert!(json.contains("cpu"), "HW profile JSON must contain cpu");
    assert!(
        json.contains("memory"),
        "HW profile JSON must contain memory"
    );
}

#[test]
fn test_e2e_llm_intent_routing() {
    let parser = IntentParser::new();

    let intent = parser.parse("show processes");
    assert_eq!(
        intent,
        UserIntent::ProcessControl {
            action: ProcessAction::List,
            target: String::new(),
        }
    );

    let intent = parser.parse("kill process 42");
    assert_eq!(
        intent,
        UserIntent::ProcessControl {
            action: ProcessAction::Kill,
            target: "42".into(),
        }
    );

    let intent = parser.parse("list blocks");
    assert_eq!(
        intent,
        UserIntent::BlockManagement {
            action: BlockAction::List,
            wasm_path: None,
            block_name: None,
        }
    );

    let intent = parser.parse("check memory");
    assert_eq!(
        intent,
        UserIntent::SystemQuery {
            metric: MetricType::Memory,
        }
    );

    let plan = parser.create_execution_plan(&intent);
    assert!(!plan.steps.is_empty() || !plan.required_capabilities.is_empty());
}

#[test]
fn test_e2e_easylang_wasm_pipeline() {
    let easylang = r#"
        # My workflow
        spawn process browser
        compact memory
    "#;

    let workflow = EasyLangParser::parse(easylang, "test_e2e").unwrap();
    assert_eq!(workflow.steps.len(), 2);
    assert_eq!(workflow.steps[0].label, "spawn_process_browser");
    assert_eq!(workflow.steps[1].label, "compact_memory");

    let wasm = WorkflowCompiler::compile_to_wasm(&workflow).unwrap();
    assert_eq!(&wasm[0..4], b"\x00asm", "Invalid WASM magic");

    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "e2e_workflow", "1.0.0", wasm).unwrap();
    assert_eq!(manifest.name, "e2e_workflow");

    let mut executor = aios_wasm::executor::BlockExecutor::with_default_config().unwrap();
    let result = executor
        .execute_block(
            &registry,
            manifest.id,
            aios_wasm::isolation::IsolationConfig::default(),
        )
        .unwrap();
    assert!(result.success);
    assert!(result.functions_called.contains(&"init".to_string()));
    assert!(result.functions_called.contains(&"start".to_string()));

    let r = executor
        .call_block_func(manifest.id, "step_count", &[])
        .unwrap();
    assert_eq!(r[0].i32(), Some(2), "step_count should be 2");

    let r = executor
        .call_block_func(manifest.id, "step_0", &[])
        .unwrap();
    assert_eq!(r[0].i32(), Some(0), "step_0 should return 0");
}

#[test]
fn test_e2e_ipc_context_ringbuf() {
    let mut bus = IpcBus::new(256);
    let mut store = EmbeddedContextStore::new(1000);
    let config = RingBufferConfig::default();
    let ringbuf = RingBuffer::new(config).unwrap();
    assert_eq!(ringbuf.capacity(), 65536);
    assert!(ringbuf.is_zero_copy());

    let pkt = IpcPacket::new(
        1,
        2,
        CommandId::SpawnProcess,
        Payload::SpawnProcess {
            name: "e2e_worker".into(),
            priority: 2,
            ram_mb: 64,
        },
    );
    let checksum = pkt.verify_checksum();
    assert!(checksum);
    bus.send(pkt).unwrap();
    let received = bus.receive().unwrap();
    assert_eq!(received.header.source_block, 1);
    assert_eq!(received.header.target_block, 2);

    store
        .telemetry_mut()
        .record(TelemetryEntry::new("e2e.test", 1.0, 64));
    assert_eq!(store.telemetry().count(), 1);
    assert!((store.telemetry().average_value("e2e.test").unwrap() - 1.0).abs() < f64::EPSILON);

    let hash = crypto::compute_sha256_bytes(b"e2e_payload");
    assert!(crypto::verify_sha256_bytes(b"e2e_payload", &hash));

    let dir = std::env::temp_dir().join("aios_e2e_persist");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let persist = PersistentStore::new(dir.join("e2e.redb"));
    let entries = vec![
        TelemetryEntry::new("e2e.persist", 42.0, 128),
        TelemetryEntry::new("e2e.persist", 43.0, 256),
    ];
    let count = persist.save_telemetry(&entries).unwrap();
    assert_eq!(count, 2);
    let loaded = persist.load_telemetry().unwrap();
    assert_eq!(loaded.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_e2e_bridge_http_endpoints() {
    let scheduler = Scheduler::new(8192);
    let registry = BlockRegistry::new();
    let acl = AccessControlLayer::new(SECRET.to_vec(), 60_000);
    let watchdog_config = WatchdogConfig {
        heartbeat_interval_ms: 1000,
        max_missed_heartbeats: 5,
        warn_threshold: 3,
        recovery_timeout_ms: 5000,
        secret: SECRET.to_vec(),
    };
    let watchdog = Watchdog::new(watchdog_config);
    let state = Arc::new(BridgeContext::new(scheduler, registry, acl, watchdog, 42));

    let port = portpicker::pick_unused_port().expect("No free port");
    let addr = format!("127.0.0.1:{port}");

    let server_state = state.clone();
    let addr_clone = addr.clone();
    tokio::spawn(async move {
        let _ = start_server(server_state, &addr_clone).await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{addr}/api/v1/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let health: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(health["status"], "ok");

    let resp = client
        .get(format!("http://{addr}/api/v1/system/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let status: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status["status"], "running");

    let resp = client
        .post(format!("http://{addr}/api/v1/workflow"))
        .json(&serde_json::json!({
            "prompts": ["show processes", "list blocks"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let workflow: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(workflow["total_steps"], 2);

    let resp = client
        .get(format!("http://{addr}/api/v1/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let metrics: serde_json::Value = resp.json().await.unwrap();
    assert!(metrics["success"].as_bool().unwrap());
    assert!(metrics["prometheus"].as_str().unwrap().contains("HELP"));

    state
        .scheduler
        .lock()
        .unwrap()
        .spawn_process("e2e_test", Priority::Normal, 64)
        .unwrap();
    let resp = client
        .post(format!("http://{addr}/api/v1/intent"))
        .json(&serde_json::json!({"prompt": "show processes"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let intent_resp: serde_json::Value = resp.json().await.unwrap();
    assert!(intent_resp["success"].as_bool().unwrap());
}

#[test]
fn test_e2e_full_orchestration_chain() {
    let profile = HardwareProfile::mock_modern();
    let ai_tier = AiTier::from_profile(&profile);
    assert_eq!(ai_tier, AiTier::Tier1);

    let mut registry = BlockRegistry::new();
    let mut scheduler = Scheduler::new(profile.memory.total_mb);
    let mut bus = IpcBus::new(256);

    let easylang = "query system status";
    let workflow = EasyLangParser::parse(easylang, "full_chain").unwrap();
    assert_eq!(workflow.steps.len(), 1);

    let wasm = WorkflowCompiler::compile_to_wasm(&workflow).unwrap();
    assert_eq!(&wasm[0..4], b"\x00asm");

    let manifest =
        BlockLoader::load_from_binary(&mut registry, "full_chain", "1.0.0", wasm).unwrap();
    assert_eq!(manifest.name, "full_chain");

    let mut executor = aios_wasm::executor::BlockExecutor::with_default_config().unwrap();
    let exec_result = executor
        .execute_block(
            &registry,
            manifest.id,
            aios_wasm::isolation::IsolationConfig::default(),
        )
        .unwrap();
    assert!(exec_result.success);

    let pid = scheduler
        .spawn_process("orchestrator", Priority::High, 256)
        .unwrap();
    let pkt = IpcPacket::new(
        manifest.id.0,
        pid.0 as u32,
        CommandId::Custom,
        Payload::Empty,
    );
    bus.send(pkt).unwrap();
    let received = bus.receive().unwrap();
    assert_eq!(received.header.source_block, manifest.id.0);

    scheduler.kill_process(pid).unwrap();
    assert_eq!(scheduler.process_count(), 0);

    let parser = IntentParser::new();
    let intent = parser.parse("show processes");
    assert!(matches!(
        intent,
        UserIntent::ProcessControl {
            action: ProcessAction::List,
            ..
        }
    ));

    let mut flight = FlightRecorder::new(100, 3600);
    flight.record(EventKind::Info, "E2E chain complete");
    assert_eq!(flight.dump().len(), 1);

    let mut collector = MetricCollector::new("e2e");
    collector.increment_counter("chain.executions", 1);
    assert_eq!(collector.get_counter("chain.executions"), 1);
}
