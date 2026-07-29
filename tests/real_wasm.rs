use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_wasm::executor::BlockExecutor;
use aios_wasm::isolation::IsolationConfig;
use std::fs;

fn sample_wasm() -> Vec<u8> {
    r#"
        (module
            (func (export "init"))
            (func (export "start"))
            (func (export "add") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add)
        )
    "#
    .as_bytes()
    .to_vec()
}

#[test]
fn test_wasm_end_to_end_load_compile_instantiate() {
    let mut registry = BlockRegistry::new();
    let binary = sample_wasm();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "e2e_block", "1.0.0", binary).unwrap();

    let mut executor = BlockExecutor::with_default_config().unwrap();
    let result = executor
        .execute_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();

    assert!(result.success);
    assert!(result.functions_called.contains(&"init".to_string()));
    assert!(result.functions_called.contains(&"start".to_string()));
}

#[test]
fn test_wasm_call_function_after_execution() {
    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "math", "1.0.0", sample_wasm()).unwrap();

    let mut executor = BlockExecutor::with_default_config().unwrap();
    executor
        .execute_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();

    let r1 = executor
        .call_block_func(
            manifest.id,
            "add",
            &[wasmtime::Val::I32(7), wasmtime::Val::I32(3)],
        )
        .unwrap();
    assert_eq!(r1[0].i32(), Some(10));

    let r2 = executor
        .call_block_func(
            manifest.id,
            "add",
            &[wasmtime::Val::I32(-100), wasmtime::Val::I32(200)],
        )
        .unwrap();
    assert_eq!(r2[0].i32(), Some(100));
}

#[test]
fn test_wasm_multiple_blocks_independent() {
    let mut registry = BlockRegistry::new();

    let wasm_a = r#"
        (module
            (func (export "get_value") (result i32) i32.const 100)
        )
    "#
    .as_bytes();
    let manifest_a =
        BlockLoader::load_from_binary(&mut registry, "block_a", "1.0.0", wasm_a.to_vec()).unwrap();

    let wasm_b = r#"
        (module
            (func (export "get_value") (result i32) i32.const 200)
        )
    "#
    .as_bytes();
    let manifest_b =
        BlockLoader::load_from_binary(&mut registry, "block_b", "1.0.0", wasm_b.to_vec()).unwrap();

    let mut executor = BlockExecutor::with_default_config().unwrap();
    executor
        .execute_block(&registry, manifest_a.id, IsolationConfig::default())
        .unwrap();
    executor
        .execute_block(&registry, manifest_b.id, IsolationConfig::default())
        .unwrap();

    let r_a = executor
        .call_block_func(manifest_a.id, "get_value", &[])
        .unwrap();
    assert_eq!(r_a[0].i32(), Some(100));

    let r_b = executor
        .call_block_func(manifest_b.id, "get_value", &[])
        .unwrap();
    assert_eq!(r_b[0].i32(), Some(200));
}

#[test]
fn test_wasm_load_from_path_end_to_end() {
    let dir = tempfile::tempdir().unwrap();

    let wasm1 = r#"
        (module
            (func (export "init"))
            (func (export "double") (param i32) (result i32)
                local.get 0
                local.get 0
                i32.add)
        )
    "#
    .as_bytes();
    fs::write(dir.path().join("doubler_1.0.0.wasm"), wasm1).unwrap();

    let wasm2 = r#"
        (module
            (func (export "init"))
            (func (export "square") (param i32) (result i32)
                local.get 0
                local.get 0
                i32.mul)
        )
    "#
    .as_bytes();
    fs::write(dir.path().join("squarer_1.0.0.wasm"), wasm2).unwrap();

    let mut registry = BlockRegistry::new();
    let mut executor = BlockExecutor::with_default_config().unwrap();
    let results =
        executor.load_from_path_and_execute(&mut registry, dir.path(), IsolationConfig::default());

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_ok()));
    assert_eq!(executor.executed_count(), 2);

    let doubler = registry.find_by_name("doubler").unwrap();
    let r = executor
        .call_block_func(doubler.manifest.id, "double", &[wasmtime::Val::I32(21)])
        .unwrap();
    assert_eq!(r[0].i32(), Some(42));

    let squarer = registry.find_by_name("squarer").unwrap();
    let r = executor
        .call_block_func(squarer.manifest.id, "square", &[wasmtime::Val::I32(7)])
        .unwrap();
    assert_eq!(r[0].i32(), Some(49));
}

#[test]
fn test_wasm_execute_all_batch() {
    let mut registry = BlockRegistry::new();

    for i in 0..5 {
        let wasm = format!(
            r#"
                (module
                    (func (export "val") (result i32) i32.const {val})
                )
            "#,
            val = i * 10
        );
        BlockLoader::load_from_binary(
            &mut registry,
            &format!("batch_{}", i),
            "1.0.0",
            wasm.into_bytes(),
        )
        .unwrap();
    }

    let mut executor = BlockExecutor::with_default_config().unwrap();
    let results = executor.execute_all(&registry, IsolationConfig::default());
    assert_eq!(results.len(), 5);
    assert!(results.iter().all(|r| r.is_ok()));
    assert_eq!(executor.executed_count(), 5);
}

#[test]
fn test_wasm_invalid_binary_fails() {
    let mut registry = BlockRegistry::new();
    BlockLoader::load_from_binary(&mut registry, "bad", "1.0.0", b"not wasm at all".to_vec())
        .unwrap();

    let mut executor = BlockExecutor::with_default_config().unwrap();
    let result = executor.execute_block(
        &registry,
        registry.find_by_name("bad").unwrap().manifest.id,
        IsolationConfig::default(),
    );
    assert!(result.is_err());
}

#[test]
fn test_wasm_execution_result_metadata() {
    let mut registry = BlockRegistry::new();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "meta_block", "3.1.4", sample_wasm()).unwrap();

    let mut executor = BlockExecutor::with_default_config().unwrap();
    let result = executor
        .execute_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();

    assert_eq!(result.name, "meta_block");
    assert_eq!(result.version, "3.1.4");
    assert_eq!(result.block_id, manifest.id);
}

#[test]
fn test_wasm_no_init_start_block() {
    let mut registry = BlockRegistry::new();
    let wasm = r#"
        (module
            (func (export "compute") (result i32) i32.const 999)
        )
    "#
    .as_bytes();
    let manifest =
        BlockLoader::load_from_binary(&mut registry, "pure", "1.0.0", wasm.to_vec()).unwrap();

    let mut executor = BlockExecutor::with_default_config().unwrap();
    let result = executor
        .execute_block(&registry, manifest.id, IsolationConfig::default())
        .unwrap();

    assert!(result.success);
    assert!(result.functions_called.is_empty());

    let r = executor
        .call_block_func(manifest.id, "compute", &[])
        .unwrap();
    assert_eq!(r[0].i32(), Some(999));
}
