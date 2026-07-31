use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_ipc::bus::IpcBus;
use aios_live_update::wasm_engine::{SwapParams, WasmLiveUpdateEngine};
use aios_wasm::isolation::IsolationConfig;

fn v1_wasm() -> Vec<u8> {
    r#"
        (module
            (func (export "init"))
            (func (export "version") (result i32) i32.const 1)
            (func (export "compute") (param i32) (result i32)
                local.get 0
                i32.const 2
                i32.mul)
        )
    "#
    .as_bytes()
    .to_vec()
}

fn v2_wasm() -> Vec<u8> {
    r#"
        (module
            (func (export "init"))
            (func (export "version") (result i32) i32.const 2)
            (func (export "compute") (param i32) (result i32)
                local.get 0
                i32.const 3
                i32.mul)
        )
    "#
    .as_bytes()
    .to_vec()
}

#[test]
fn test_deploy_and_call_wasm_block() {
    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "deploy_test", "1.0.0", v1_wasm()).unwrap();

    let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();

    let deploy = engine
        .deploy_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();
    assert_eq!(deploy.name, "deploy_test");
    assert!(engine.is_active(manifest.id));
    assert_eq!(engine.active_count(), 1);

    let r = engine
        .call_block_func(manifest.id, "compute", &[wasmtime::Val::I32(10)])
        .unwrap();
    assert_eq!(r[0].i32(), Some(20));
}

#[test]
fn test_hot_swap_version_change() {
    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "swapper", "1.0.0", v1_wasm()).unwrap();

    let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
    let mut queue = IpcBus::new(1024);

    engine
        .deploy_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();

    let r = engine.call_block_func(manifest.id, "version", &[]).unwrap();
    assert_eq!(r[0].i32(), Some(1));

    let swap_result = engine
        .swap_block(
            &mut registry,
            manifest.id,
            SwapParams {
                new_binary: v2_wasm(),
                new_version: "2.0.0".to_string(),
                health_check: None,
                isolation: IsolationConfig::default(),
            },
            &mut queue,
        )
        .unwrap();
    assert_eq!(swap_result.old_version, "1.0.0");
    assert_eq!(swap_result.new_version, "2.0.0");

    let r = engine.call_block_func(manifest.id, "version", &[]).unwrap();
    assert_eq!(r[0].i32(), Some(2));

    let r = engine
        .call_block_func(manifest.id, "compute", &[wasmtime::Val::I32(5)])
        .unwrap();
    assert_eq!(r[0].i32(), Some(15));
}

#[test]
fn test_hot_swap_rollback() {
    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "rollback_test", "1.0.0", v1_wasm()).unwrap();

    let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
    let mut queue = IpcBus::new(1024);

    engine
        .deploy_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();
    engine
        .swap_block(
            &mut registry,
            manifest.id,
            SwapParams {
                new_binary: v2_wasm(),
                new_version: "2.0.0".to_string(),
                health_check: None,
                isolation: IsolationConfig::default(),
            },
            &mut queue,
        )
        .unwrap();

    let rollback = engine.rollback_block(manifest.id, &mut queue).unwrap();
    assert_eq!(rollback.restored_version, "1.0.0");
}

#[test]
fn test_hot_swap_with_health_check_pass() {
    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "hc_pass", "1.0.0", v1_wasm()).unwrap();

    let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
    let mut queue = IpcBus::new(1024);

    engine
        .deploy_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();

    let health_ok: aios_live_update::engine::HealthCheckFn = Box::new(|_binary| true);
    let result = engine.swap_block(
        &mut registry,
        manifest.id,
        SwapParams {
            new_binary: v2_wasm(),
            new_version: "2.0.0".to_string(),
            health_check: Some(health_ok),
            isolation: IsolationConfig::default(),
        },
        &mut queue,
    );
    assert!(result.is_ok());
}

#[test]
fn test_hot_swap_with_health_check_fail() {
    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "hc_fail", "1.0.0", v1_wasm()).unwrap();

    let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
    let mut queue = IpcBus::new(1024);

    engine
        .deploy_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();

    let health_fail: aios_live_update::engine::HealthCheckFn = Box::new(|_binary| false);
    let result = engine.swap_block(
        &mut registry,
        manifest.id,
        SwapParams {
            new_binary: v2_wasm(),
            new_version: "2.0.0".to_string(),
            health_check: Some(health_fail),
            isolation: IsolationConfig::default(),
        },
        &mut queue,
    );
    assert!(result.is_err());
}

#[test]
fn test_swap_history_recorded() {
    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "history", "1.0.0", v1_wasm()).unwrap();

    let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
    let mut queue = IpcBus::new(1024);

    engine
        .deploy_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();
    engine
        .swap_block(
            &mut registry,
            manifest.id,
            SwapParams {
                new_binary: v2_wasm(),
                new_version: "2.0.0".to_string(),
                health_check: None,
                isolation: IsolationConfig::default(),
            },
            &mut queue,
        )
        .unwrap();

    let history = engine.swap_history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].old_version, "1.0.0");
    assert_eq!(history[0].new_version, "2.0.0");
    assert!(history[0].success);
}

#[test]
fn test_deploy_multiple_swap_one() {
    let mut registry = BlockRegistry::new();
    let m1 = BlockLoader::load_from_binary(&mut registry, "block_a", "1.0.0", v1_wasm()).unwrap();
    let m2 = BlockLoader::load_from_binary(&mut registry, "block_b", "1.0.0", v1_wasm()).unwrap();

    let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
    let mut queue = IpcBus::new(1024);

    engine
        .deploy_block(&registry, m1.id, IsolationConfig::default())
        .unwrap();
    engine
        .deploy_block(&registry, m2.id, IsolationConfig::default())
        .unwrap();
    assert_eq!(engine.active_count(), 2);

    engine
        .swap_block(
            &mut registry,
            m1.id,
            SwapParams {
                new_binary: v2_wasm(),
                new_version: "2.0.0".to_string(),
                health_check: None,
                isolation: IsolationConfig::default(),
            },
            &mut queue,
        )
        .unwrap();

    let r1 = engine.call_block_func(m1.id, "version", &[]).unwrap();
    assert_eq!(r1[0].i32(), Some(2));

    let r2 = engine.call_block_func(m2.id, "version", &[]).unwrap();
    assert_eq!(r2[0].i32(), Some(1));
}
