use crate::isolation::IsolationConfig;
use crate::sandbox::{SandboxConfig, StoreState, WasmBlock, WasmSandbox};
use aios_block_mgr::registry::BlockRegistry;
use aios_core::block::BlockId;
use aios_core::error::{AIOSException, Result};
use std::collections::HashMap;
use std::path::Path;
use wasmtime::Store;

pub struct BlockExecutor {
    sandbox: WasmSandbox,
    executed: HashMap<BlockId, (WasmBlock, Store<StoreState>)>,
}

impl BlockExecutor {
    pub fn new(sandbox_config: SandboxConfig) -> Result<Self> {
        let sandbox = WasmSandbox::new(sandbox_config)?;
        Ok(Self {
            sandbox,
            executed: HashMap::new(),
        })
    }

    pub fn with_default_config() -> Result<Self> {
        Self::new(SandboxConfig::default())
    }

    pub fn execute_block(
        &mut self,
        registry: &BlockRegistry,
        id: BlockId,
        isolation: IsolationConfig,
    ) -> Result<ExecutionResult> {
        let entry = registry.get(id)?;
        let binary = entry.binary.clone();
        let name = entry.manifest.name.clone();
        let version = entry.manifest.version.clone();

        log::info!(
            "BlockExecutor: Executing block '{}' v{} ({} bytes)",
            name,
            version,
            binary.len()
        );

        let mut wasm_block = WasmBlock::new(
            name.clone(),
            version.clone(),
            &binary,
            self.sandbox.config().clone(),
            isolation,
        )?;

        let mut store = wasm_block.create_store()?;
        wasm_block.instantiate(&mut store)?;

        let mut result = ExecutionResult {
            block_id: id,
            name: name.clone(),
            version,
            functions_called: Vec::new(),
            success: true,
        };

        if let Some(init_func) = wasm_block
            .instance_ref()
            .and_then(|inst| inst.get_func(&mut store, "init"))
        {
            match init_func.call(&mut store, &[], &mut []) {
                Ok(_) => {
                    log::info!("BlockExecutor: Called init() on block '{}'", name);
                    result.functions_called.push("init".to_string());
                }
                Err(e) => {
                    log::warn!("BlockExecutor: init() failed on block '{}': {}", name, e);
                }
            }
        }

        if let Some(start_func) = wasm_block
            .instance_ref()
            .and_then(|inst| inst.get_func(&mut store, "start"))
        {
            match start_func.call(&mut store, &[], &mut []) {
                Ok(_) => {
                    log::info!("BlockExecutor: Called start() on block '{}'", name);
                    result.functions_called.push("start".to_string());
                }
                Err(e) => {
                    log::warn!("BlockExecutor: start() failed on block '{}': {}", name, e);
                }
            }
        }

        self.executed.insert(id, (wasm_block, store));
        log::info!(
            "BlockExecutor: Block '{}' executed successfully (functions: {:?})",
            name,
            result.functions_called
        );

        Ok(result)
    }

    pub fn execute_all(
        &mut self,
        registry: &BlockRegistry,
        isolation: IsolationConfig,
    ) -> Vec<Result<ExecutionResult>> {
        let ids: Vec<BlockId> = registry.all_ids();
        let mut results = Vec::new();

        for id in ids {
            results.push(self.execute_block(registry, id, isolation.clone()));
        }

        results
    }

    pub fn call_block_func(
        &mut self,
        id: BlockId,
        func_name: &str,
        args: &[wasmtime::Val],
    ) -> Result<Vec<wasmtime::Val>> {
        let (block, store) = self
            .executed
            .get_mut(&id)
            .ok_or_else(|| AIOSException::BlockNotFound(format!("Block {} not executed", id)))?;

        block.call_func(store, func_name, args)
    }

    pub fn executed_count(&self) -> usize {
        self.executed.len()
    }

    pub fn is_executed(&self, id: BlockId) -> bool {
        self.executed.contains_key(&id)
    }

    pub fn executed_ids(&self) -> Vec<BlockId> {
        self.executed.keys().copied().collect()
    }

    pub fn load_from_path_and_execute(
        &mut self,
        registry: &mut BlockRegistry,
        dir: &Path,
        isolation: IsolationConfig,
    ) -> Vec<Result<ExecutionResult>> {
        log::info!("BlockExecutor: Loading and executing blocks from {:?}", dir);
        let load_results = registry.load_from_path(dir);
        let mut exec_results = Vec::new();

        for load_result in load_results {
            match load_result {
                Ok(manifest) => {
                    let exec = self.execute_block(registry, manifest.id, isolation.clone());
                    exec_results.push(exec);
                }
                Err(e) => {
                    log::error!("BlockExecutor: Failed to load block: {}", e);
                    exec_results.push(Err(e));
                }
            }
        }

        let ok_count = exec_results.iter().filter(|r| r.is_ok()).count();
        log::info!(
            "BlockExecutor: Loaded and executed {}/{} blocks from {:?}",
            ok_count,
            exec_results.len(),
            dir
        );

        exec_results
    }

    pub fn sandbox(&self) -> &WasmSandbox {
        &self.sandbox
    }
}

#[derive(Debug)]
pub struct ExecutionResult {
    pub block_id: BlockId,
    pub name: String,
    pub version: String,
    pub functions_called: Vec<String>,
    pub success: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_block_mgr::loader::BlockLoader;

    fn sample_wasm_binary() -> Vec<u8> {
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
    fn test_execute_block_with_init_start() {
        let mut registry = BlockRegistry::new();
        let binary = sample_wasm_binary();
        let manifest =
            BlockLoader::load_from_binary(&mut registry, "test_block", "1.0.0", binary).unwrap();

        let mut executor = BlockExecutor::with_default_config().unwrap();
        let result = executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();

        assert!(result.success);
        assert!(result.functions_called.contains(&"init".to_string()));
        assert!(result.functions_called.contains(&"start".to_string()));
        assert!(executor.is_executed(manifest.id));
        assert_eq!(executor.executed_count(), 1);
    }

    #[test]
    fn test_execute_block_call_function_after() {
        let mut registry = BlockRegistry::new();
        let binary = sample_wasm_binary();
        let manifest =
            BlockLoader::load_from_binary(&mut registry, "adder", "1.0.0", binary).unwrap();

        let mut executor = BlockExecutor::with_default_config().unwrap();
        executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();

        let results = executor
            .call_block_func(
                manifest.id,
                "add",
                &[wasmtime::Val::I32(10), wasmtime::Val::I32(20)],
            )
            .unwrap();
        assert_eq!(results[0].i32(), Some(30));
    }

    #[test]
    fn test_execute_block_no_init_start() {
        let mut registry = BlockRegistry::new();
        let binary = r#"
            (module
                (func (export "compute") (result i32)
                    i32.const 99)
            )
        "#
        .as_bytes()
        .to_vec();

        let manifest =
            BlockLoader::load_from_binary(&mut registry, "no_init", "1.0.0", binary).unwrap();

        let mut executor = BlockExecutor::with_default_config().unwrap();
        let result = executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();

        assert!(result.success);
        assert!(result.functions_called.is_empty());
    }

    #[test]
    fn test_execute_all() {
        let mut registry = BlockRegistry::new();
        let binary = sample_wasm_binary();

        BlockLoader::load_from_binary(&mut registry, "block_a", "1.0.0", binary.clone()).unwrap();
        BlockLoader::load_from_binary(&mut registry, "block_b", "1.0.0", binary).unwrap();

        let mut executor = BlockExecutor::with_default_config().unwrap();
        let results = executor.execute_all(&registry, IsolationConfig::default());
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));
        assert_eq!(executor.executed_count(), 2);
    }

    #[test]
    fn test_call_nonexistent_block_fails() {
        let mut executor = BlockExecutor::with_default_config().unwrap();
        let result = executor.call_block_func(BlockId::new(999), "foo", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_execution_result_fields() {
        let mut registry = BlockRegistry::new();
        let binary = sample_wasm_binary();
        let manifest =
            BlockLoader::load_from_binary(&mut registry, "check", "2.0.0", binary).unwrap();

        let mut executor = BlockExecutor::with_default_config().unwrap();
        let result = executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();

        assert_eq!(result.block_id, manifest.id);
        assert_eq!(result.name, "check");
        assert_eq!(result.version, "2.0.0");
    }

    #[test]
    fn test_load_from_path_and_execute() {
        let dir = tempfile::tempdir().unwrap();

        let wasm1 = r#"
            (module
                (func (export "init"))
                (func (export "start"))
                (func (export "add") (param i32 i32) (result i32)
                    local.get 0
                    local.get 1
                    i32.add)
            )
        "#
        .as_bytes();
        std::fs::write(dir.path().join("math_1.0.0.wasm"), wasm1).unwrap();

        let wasm2 = r#"
            (module
                (func (export "init"))
                (func (export "multiply") (param i32 i32) (result i32)
                    local.get 0
                    local.get 1
                    i32.mul)
            )
        "#
        .as_bytes();
        std::fs::write(dir.path().join("calc_1.0.0.wasm"), wasm2).unwrap();

        let mut registry = BlockRegistry::new();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        let results = executor.load_from_path_and_execute(
            &mut registry,
            dir.path(),
            IsolationConfig::default(),
        );

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));
        assert_eq!(executor.executed_count(), 2);

        let math_entry = registry.find_by_name("math").unwrap();
        let calc_entry = registry.find_by_name("calc").unwrap();

        let math_result = results
            .iter()
            .find(|r| r.as_ref().map(|r| r.name == "math").unwrap_or(false))
            .unwrap()
            .as_ref()
            .unwrap();
        assert!(math_result.functions_called.contains(&"init".to_string()));
        assert!(math_result.functions_called.contains(&"start".to_string()));

        let calc_result = results
            .iter()
            .find(|r| r.as_ref().map(|r| r.name == "calc").unwrap_or(false))
            .unwrap()
            .as_ref()
            .unwrap();
        assert!(calc_result.functions_called.contains(&"init".to_string()));

        let add_result = executor
            .call_block_func(
                math_entry.manifest.id,
                "add",
                &[wasmtime::Val::I32(5), wasmtime::Val::I32(7)],
            )
            .unwrap();
        assert_eq!(add_result[0].i32(), Some(12));

        let mul_result = executor
            .call_block_func(
                calc_entry.manifest.id,
                "multiply",
                &[wasmtime::Val::I32(6), wasmtime::Val::I32(8)],
            )
            .unwrap();
        assert_eq!(mul_result[0].i32(), Some(48));
    }

    #[test]
    fn test_load_from_path_and_execute_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = BlockRegistry::new();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        let results = executor.load_from_path_and_execute(
            &mut registry,
            dir.path(),
            IsolationConfig::default(),
        );
        assert!(results.is_empty());
        assert_eq!(executor.executed_count(), 0);
    }

    #[test]
    fn test_load_from_path_and_execute_invalid_wasm() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad_1.0.0.wasm"), b"not valid wasm").unwrap();

        let mut registry = BlockRegistry::new();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        let results = executor.load_from_path_and_execute(
            &mut registry,
            dir.path(),
            IsolationConfig::default(),
        );

        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
        assert_eq!(executor.executed_count(), 0);
    }

    #[test]
    fn test_load_from_path_and_execute_mixed_valid_invalid() {
        let dir = tempfile::tempdir().unwrap();

        let good_wasm = r#"
            (module
                (func (export "init"))
            )
        "#
        .as_bytes();
        std::fs::write(dir.path().join("good_1.0.0.wasm"), good_wasm).unwrap();
        std::fs::write(dir.path().join("bad_1.0.0.wasm"), b"garbage data").unwrap();

        let mut registry = BlockRegistry::new();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        let results = executor.load_from_path_and_execute(
            &mut registry,
            dir.path(),
            IsolationConfig::default(),
        );

        assert_eq!(results.len(), 2);
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok_count, 1);
        assert_eq!(executor.executed_count(), 1);
    }
}
