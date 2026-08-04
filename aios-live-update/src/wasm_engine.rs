use crate::engine::{HealthCheckFn, LiveUpdateEngine, SwapRecord};
use aios_block_mgr::registry::BlockRegistry;
use aios_core::block::BlockId;
use aios_core::error::{AIOSException, Result};
use aios_ipc::bus::IpcBus;
use aios_wasm::isolation::IsolationConfig;
use aios_wasm::sandbox::{SandboxConfig, StoreState, WasmBlock, WasmSandbox};
use std::collections::HashMap;
use wasmtime::Store;

pub struct WasmLiveUpdateEngine {
    inner: LiveUpdateEngine,
    sandbox: WasmSandbox,
    active_blocks: HashMap<BlockId, (WasmBlock, Store<StoreState>)>,
}

impl WasmLiveUpdateEngine {
    pub fn new(rollback_timeout_ms: u64, sandbox_config: SandboxConfig) -> Result<Self> {
        let sandbox = WasmSandbox::new(sandbox_config)?;
        Ok(Self {
            inner: LiveUpdateEngine::new(rollback_timeout_ms),
            sandbox,
            active_blocks: HashMap::new(),
        })
    }

    pub fn with_defaults() -> Result<Self> {
        Self::new(30_000, SandboxConfig::default())
    }

    pub fn deploy_block(
        &mut self,
        registry: &BlockRegistry,
        id: BlockId,
        isolation: IsolationConfig,
    ) -> Result<DeployResult> {
        let entry = registry.get(id)?;
        let binary = entry.binary.clone();
        let name = entry.manifest.name.clone();
        let version = entry.manifest.version.clone();

        log::info!(
            "WasmLiveUpdate: Deploying WASM block '{}' v{} ({} bytes)",
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

        let mut functions_called = Vec::new();
        if let Some(init) = wasm_block
            .instance_ref()
            .and_then(|inst| inst.get_func(&mut store, "init"))
        {
            if init.call(&mut store, &[], &mut []).is_ok() {
                functions_called.push("init".to_string());
            }
        }
        if let Some(start) = wasm_block
            .instance_ref()
            .and_then(|inst| inst.get_func(&mut store, "start"))
        {
            if start.call(&mut store, &[], &mut []).is_ok() {
                functions_called.push("start".to_string());
            }
        }

        self.active_blocks.insert(id, (wasm_block, store));

        log::info!(
            "WasmLiveUpdate: Block '{}' deployed (functions: {:?})",
            name,
            functions_called
        );

        Ok(DeployResult {
            block_id: id,
            name,
            version,
            functions_called,
        })
    }

    pub fn swap_block(
        &mut self,
        registry: &mut BlockRegistry,
        id: BlockId,
        params: SwapParams,
        queue: &mut IpcBus,
    ) -> Result<SwapResult> {
        let SwapParams {
            new_binary,
            new_version,
            health_check,
            isolation,
        } = params;
        let old_entry = registry.get(id)?;
        let old_binary = old_entry.binary.clone();
        let old_version = old_entry.manifest.version.clone();

        let old_memory = if let Some((old_block, old_store)) = self.active_blocks.get_mut(&id) {
            old_block.extract_linear_memory(old_store)
        } else {
            None
        };

        self.inner.perform_swap(
            id.0,
            old_binary.clone(),
            old_version.clone(),
            b"state".to_vec(),
            new_binary.clone(),
            new_version.to_string(),
            aios_core::crypto::compute_sha256_bytes(&new_binary),
            queue,
            health_check.as_ref(),
        )?;

        let mut wasm_block = WasmBlock::new(
            old_entry.manifest.name.clone(),
            new_version.to_string(),
            &new_binary,
            self.sandbox.config().clone(),
            isolation,
        )?;

        let mut store = wasm_block.create_store()?;
        wasm_block.instantiate(&mut store)?;

        if let Some(ref mem_data) = old_memory {
            if !wasm_block.restore_linear_memory(&mut store, mem_data) {
                log::warn!(
                    "WasmLiveUpdate: state restore failed for '{}' — memory was not migrated",
                    old_entry.manifest.name
                );
            }
        }

        let mut functions_called = Vec::new();
        if let Some(init) = wasm_block
            .instance_ref()
            .and_then(|inst| inst.get_func(&mut store, "init"))
        {
            if init.call(&mut store, &[], &mut []).is_ok() {
                functions_called.push("init".to_string());
            }
        }

        self.active_blocks.insert(id, (wasm_block, store));

        log::info!(
            "WasmLiveUpdate: Block '{}' swapped v{} → v{} (memory migrated: {})",
            old_entry.manifest.name,
            old_version,
            new_version,
            old_memory.is_some()
        );

        Ok(SwapResult {
            block_id: id,
            old_version,
            new_version: new_version.to_string(),
            functions_called,
            memory_migrated: old_memory.is_some(),
        })
    }

    pub fn rollback_block(&mut self, id: BlockId, queue: &mut IpcBus) -> Result<RollbackResult> {
        self.active_blocks.remove(&id);

        let entry = self.inner.rollback(id.0, queue)?;

        Ok(RollbackResult {
            block_id: id,
            restored_version: entry.old_version,
            restored_binary_len: entry.old_binary.len(),
        })
    }

    pub fn call_block_func(
        &mut self,
        id: BlockId,
        func_name: &str,
        args: &[wasmtime::Val],
    ) -> Result<Vec<wasmtime::Val>> {
        let (block, store) = self
            .active_blocks
            .get_mut(&id)
            .ok_or_else(|| AIOSException::BlockNotFound(format!("Block {} not deployed", id)))?;

        block.call_func(store, func_name, args)
    }

    pub fn active_count(&self) -> usize {
        self.active_blocks.len()
    }

    pub fn is_active(&self, id: BlockId) -> bool {
        self.active_blocks.contains_key(&id)
    }

    pub fn swap_history(&self) -> &[SwapRecord] {
        self.inner.swap_history()
    }

    pub fn pending_rollbacks(&self) -> Vec<(u32, &str)> {
        self.inner.pending_rollbacks()
    }

    pub fn engine(&self) -> &LiveUpdateEngine {
        &self.inner
    }

    pub fn engine_mut(&mut self) -> &mut LiveUpdateEngine {
        &mut self.inner
    }

    pub fn reroute_pending(
        &self,
        queue: &mut IpcBus,
        old_block: BlockId,
        new_block: BlockId,
    ) -> Result<usize> {
        let mut snapshot = crate::state_transfer::StateTransferManager::extract_state(queue, &[])?;
        let count = crate::state_transfer::StateTransferManager::reroute_snapshot(
            &mut snapshot,
            old_block.0,
            new_block.0,
        );
        let _ = crate::state_transfer::StateTransferManager::restore_state(queue, snapshot);
        Ok(count)
    }
}

pub struct SwapParams {
    pub new_binary: Vec<u8>,
    pub new_version: String,
    pub health_check: Option<HealthCheckFn>,
    pub isolation: IsolationConfig,
}

#[derive(Debug)]
pub struct DeployResult {
    pub block_id: BlockId,
    pub name: String,
    pub version: String,
    pub functions_called: Vec<String>,
}

#[derive(Debug)]
pub struct SwapResult {
    pub block_id: BlockId,
    pub old_version: String,
    pub new_version: String,
    pub functions_called: Vec<String>,
    pub memory_migrated: bool,
}

#[derive(Debug)]
pub struct RollbackResult {
    pub block_id: BlockId,
    pub restored_version: String,
    pub restored_binary_len: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_block_mgr::loader::BlockLoader;

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

    fn sample_wasm_v2() -> Vec<u8> {
        r#"
            (module
                (func (export "init"))
                (func (export "start"))
                (func (export "add") (param i32 i32) (result i32)
                    local.get 0
                    local.get 1
                    i32.mul)
            )
        "#
        .as_bytes()
        .to_vec()
    }

    #[test]
    fn test_deploy_block() {
        let mut reg = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "math", "1.0.0", sample_wasm()).unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        let result = engine
            .deploy_block(&reg, manifest.id, IsolationConfig::default())
            .unwrap();

        assert_eq!(result.name, "math");
        assert!(result.functions_called.contains(&"init".to_string()));
        assert!(result.functions_called.contains(&"start".to_string()));
        assert!(engine.is_active(manifest.id));
        assert_eq!(engine.active_count(), 1);
    }

    #[test]
    fn test_call_deployed_block() {
        let mut reg = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "calc", "1.0.0", sample_wasm()).unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        engine
            .deploy_block(&reg, manifest.id, IsolationConfig::default())
            .unwrap();

        let results = engine
            .call_block_func(
                manifest.id,
                "add",
                &[wasmtime::Val::I32(6), wasmtime::Val::I32(7)],
            )
            .unwrap();
        assert_eq!(results[0].i32(), Some(13));
    }

    #[test]
    fn test_swap_block_real_wasm() {
        let mut reg = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "math", "1.0.0", sample_wasm()).unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        engine
            .deploy_block(&reg, manifest.id, IsolationConfig::default())
            .unwrap();

        let results_before = engine
            .call_block_func(
                manifest.id,
                "add",
                &[wasmtime::Val::I32(3), wasmtime::Val::I32(4)],
            )
            .unwrap();
        assert_eq!(results_before[0].i32(), Some(7));

        let mut bus = IpcBus::new(10);
        let swap_result = engine
            .swap_block(
                &mut reg,
                manifest.id,
                SwapParams {
                    new_binary: sample_wasm_v2(),
                    new_version: "2.0.0".to_string(),
                    health_check: None,
                    isolation: IsolationConfig::default(),
                },
                &mut bus,
            )
            .unwrap();

        assert_eq!(swap_result.old_version, "1.0.0");
        assert_eq!(swap_result.new_version, "2.0.0");

        let results_after = engine
            .call_block_func(
                manifest.id,
                "add",
                &[wasmtime::Val::I32(3), wasmtime::Val::I32(4)],
            )
            .unwrap();
        assert_eq!(results_after[0].i32(), Some(12));
    }

    #[test]
    fn test_rollback_block_real() {
        let mut reg = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "math", "1.0.0", sample_wasm()).unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        engine
            .deploy_block(&reg, manifest.id, IsolationConfig::default())
            .unwrap();

        let mut bus = IpcBus::new(10);
        engine
            .swap_block(
                &mut reg,
                manifest.id,
                SwapParams {
                    new_binary: sample_wasm_v2(),
                    new_version: "2.0.0".to_string(),
                    health_check: None,
                    isolation: IsolationConfig::default(),
                },
                &mut bus,
            )
            .unwrap();

        assert!(engine.is_active(manifest.id));

        let rollback = engine.rollback_block(manifest.id, &mut bus).unwrap();
        assert_eq!(rollback.restored_version, "1.0.0");
        assert!(!engine.is_active(manifest.id));
    }

    #[test]
    fn test_swap_with_failing_health_check() {
        let mut reg = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "math", "1.0.0", sample_wasm()).unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        engine
            .deploy_block(&reg, manifest.id, IsolationConfig::default())
            .unwrap();

        let mut bus = IpcBus::new(10);
        let failing: HealthCheckFn = Box::new(|_: &[u8]| false);

        let result = engine.swap_block(
            &mut reg,
            manifest.id,
            SwapParams {
                new_binary: sample_wasm_v2(),
                new_version: "2.0.0".to_string(),
                health_check: Some(failing),
                isolation: IsolationConfig::default(),
            },
            &mut bus,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_swap_history_tracking() {
        let mut reg = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "math", "1.0.0", sample_wasm()).unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        engine
            .deploy_block(&reg, manifest.id, IsolationConfig::default())
            .unwrap();

        let mut bus = IpcBus::new(10);
        engine
            .swap_block(
                &mut reg,
                manifest.id,
                SwapParams {
                    new_binary: sample_wasm_v2(),
                    new_version: "2.0.0".to_string(),
                    health_check: None,
                    isolation: IsolationConfig::default(),
                },
                &mut bus,
            )
            .unwrap();

        engine.rollback_block(manifest.id, &mut bus).unwrap();

        let history = engine.swap_history();
        assert_eq!(history.len(), 2);
        assert!(history[0].success);
        assert!(history[1].rolled_back);
    }

    fn sample_wasm_with_memory() -> Vec<u8> {
        r#"
            (module
                (memory (export "memory") 1 4)
                (func (export "init"))
                (func (export "start"))
                (func (export "write_val") (param i32)
                    (i32.store (i32.const 0) (local.get 0)))
                (func (export "read_val") (result i32)
                    (i32.load (i32.const 0)))
            )
        "#
        .as_bytes()
        .to_vec()
    }

    fn sample_wasm_with_memory_v2() -> Vec<u8> {
        r#"
            (module
                (memory (export "memory") 1 4)
                (func (export "init"))
                (func (export "start"))
                (func (export "write_val") (param i32)
                    (i32.store (i32.const 0) (local.get 0)))
                (func (export "read_val") (result i32)
                    (i32.load (i32.const 0)))
                (func (export "add") (param i32 i32) (result i32)
                    local.get 0
                    local.get 1
                    i32.mul)
            )
        "#
        .as_bytes()
        .to_vec()
    }

    #[test]
    fn test_swap_migrates_linear_memory() {
        let mut reg = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "migr", "1.0.0", sample_wasm_with_memory())
                .unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        engine
            .deploy_block(&reg, manifest.id, IsolationConfig::default())
            .unwrap();

        engine
            .call_block_func(manifest.id, "write_val", &[wasmtime::Val::I32(42)])
            .unwrap();
        let val_before = engine
            .call_block_func(manifest.id, "read_val", &[])
            .unwrap();
        assert_eq!(val_before[0].i32(), Some(42));

        let mut bus = IpcBus::new(10);
        let result = engine
            .swap_block(
                &mut reg,
                manifest.id,
                SwapParams {
                    new_binary: sample_wasm_with_memory_v2(),
                    new_version: "2.0.0".to_string(),
                    health_check: None,
                    isolation: IsolationConfig::default(),
                },
                &mut bus,
            )
            .unwrap();

        assert!(result.memory_migrated);
        let val_after = engine
            .call_block_func(manifest.id, "read_val", &[])
            .unwrap();
        assert_eq!(val_after[0].i32(), Some(42));
    }

    #[test]
    fn test_swap_without_memory_no_migration() {
        let mut reg = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "nomem", "1.0.0", sample_wasm()).unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        engine
            .deploy_block(&reg, manifest.id, IsolationConfig::default())
            .unwrap();

        let mut bus = IpcBus::new(10);
        let result = engine
            .swap_block(
                &mut reg,
                manifest.id,
                SwapParams {
                    new_binary: sample_wasm_v2(),
                    new_version: "2.0.0".to_string(),
                    health_check: None,
                    isolation: IsolationConfig::default(),
                },
                &mut bus,
            )
            .unwrap();

        assert!(!result.memory_migrated);
    }

    #[test]
    fn test_reroute_pending_packets() {
        let mut reg = BlockRegistry::new();
        let manifest_a =
            BlockLoader::load_from_binary(&mut reg, "block_a", "1.0.0", sample_wasm()).unwrap();
        let manifest_b =
            BlockLoader::load_from_binary(&mut reg, "block_b", "1.0.0", sample_wasm_v2()).unwrap();

        let mut engine = WasmLiveUpdateEngine::with_defaults().unwrap();
        engine
            .deploy_block(&reg, manifest_a.id, IsolationConfig::default())
            .unwrap();
        engine
            .deploy_block(&reg, manifest_b.id, IsolationConfig::default())
            .unwrap();

        let mut bus = IpcBus::new(10);
        bus.send(aios_core::ipc_protocol::IpcPacket::new(
            0,
            manifest_a.id.0,
            aios_core::ipc_protocol::CommandId::HealthCheck,
            aios_core::ipc_protocol::Payload::Empty,
        ))
        .unwrap();
        bus.send(aios_core::ipc_protocol::IpcPacket::new(
            0,
            manifest_b.id.0,
            aios_core::ipc_protocol::CommandId::HealthCheck,
            aios_core::ipc_protocol::Payload::Empty,
        ))
        .unwrap();
        bus.send(aios_core::ipc_protocol::IpcPacket::new(
            0,
            manifest_a.id.0,
            aios_core::ipc_protocol::CommandId::HealthCheck,
            aios_core::ipc_protocol::Payload::Empty,
        ))
        .unwrap();

        let rerouted = engine
            .reroute_pending(&mut bus, manifest_a.id, manifest_b.id)
            .unwrap();
        assert_eq!(rerouted, 2);

        let p1 = bus.receive().unwrap();
        assert_eq!(p1.header.target_block, manifest_b.id.0);
        let p2 = bus.receive().unwrap();
        assert_eq!(p2.header.target_block, manifest_b.id.0);
        let p3 = bus.receive().unwrap();
        assert_eq!(p3.header.target_block, manifest_b.id.0);
    }
}
