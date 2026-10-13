use crate::isolation::IsolationConfig;
use crate::sandbox::{SandboxConfig, StoreState, WasmBlock, WasmSandbox};
use aios_block_mgr::registry::BlockRegistry;
use aios_core::block::BlockId;
use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::IpcPacket;
use std::collections::HashMap;
use std::path::Path;
use wasmtime::Store;

/// WebAssembly linear-memory page size in bytes.
const WASM_PAGE: usize = 64 * 1024;

/// Upper bound on an IPC request staged into guest memory.
const MAX_IPC_REQUEST: usize = 4 * 1024 * 1024;

/// Upper bound on an IPC response read back from guest memory, guarding against
/// a corrupt length field.
const MAX_IPC_RESPONSE: usize = 4 * 1024 * 1024;

/// Exported function a WASM block must provide to receive IPC packets.
pub const IPC_ENTRY_EXPORT: &str = "handle_message";

/// Optional exported guest allocator used to stage an IPC request.
pub const IPC_ALLOC_EXPORT: &str = "aios_alloc";

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
            wasm_block.arm_timeout(&mut store);
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
            wasm_block.arm_timeout(&mut store);
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

    /// Whether an executed block exports `func_name` (e.g. `handle_message`).
    pub fn has_export(&mut self, id: BlockId, func_name: &str) -> bool {
        match self.executed.get_mut(&id) {
            Some((block, store)) => block.has_func(store, func_name),
            None => false,
        }
    }

    /// Whether an executed block can receive IPC packets (exports
    /// [`IPC_ENTRY_EXPORT`]).
    pub fn routes_ipc(&mut self, id: BlockId) -> bool {
        self.has_export(id, IPC_ENTRY_EXPORT)
    }

    /// Deliver a bincode-serialized [`IpcPacket`] to an executed WASM block and
    /// deserialize its (optional) response.
    ///
    /// This is the WASM twin of [`aios_core::block::StatefulBlock::handle_message`]:
    /// the kernel's `MessageRouter` maps an incoming packet onto the block's
    /// exported [`IPC_ENTRY_EXPORT`] function so disk blocks participate in IPC
    /// exactly like the compiled-in base blocks.
    pub fn dispatch_wasm_packet(
        &mut self,
        id: BlockId,
        packet: &IpcPacket,
    ) -> Result<Option<IpcPacket>> {
        let request = bincode::serialize(packet)
            .map_err(|e| AIOSException::Generic(format!("WASM IPC serialize: {e}")))?;
        match self.dispatch_wasm(id, &request)? {
            None => Ok(None),
            Some(bytes) => {
                let packet = bincode::deserialize(&bytes)
                    .map_err(|e| AIOSException::Generic(format!("WASM IPC deserialize: {e}")))?;
                Ok(Some(packet))
            }
        }
    }

    /// Stage `request` in the block's linear memory, call its
    /// [`IPC_ENTRY_EXPORT`] function and read the response back.
    ///
    /// The block returns a packed `i64` `(ptr << 32) | len` pointing at a
    /// bincode-serialized `IpcPacket`; `len == 0` means "no response". The input
    /// is staged through the guest's `aios_alloc` export when present, otherwise
    /// in host-grown scratch memory that the block reads but must not own.
    pub fn dispatch_wasm(&mut self, id: BlockId, request: &[u8]) -> Result<Option<Vec<u8>>> {
        let (block, store) = self
            .executed
            .get_mut(&id)
            .ok_or_else(|| AIOSException::BlockNotFound(format!("Block {} not executed", id)))?;

        if !block.has_func(store, IPC_ENTRY_EXPORT) {
            return Err(AIOSException::Generic(format!(
                "Block '{}' does not export '{}'",
                block.name(),
                IPC_ENTRY_EXPORT
            )));
        }

        let ptr = stage_request(block, store, request)?;
        let results = block.call_func(
            store,
            IPC_ENTRY_EXPORT,
            &[
                wasmtime::Val::I32(ptr as i32),
                wasmtime::Val::I32(request.len() as i32),
            ],
        )?;

        let packed = results.first().and_then(|v| v.i64()).unwrap_or(0) as u64;
        let resp_len = (packed & 0xFFFF_FFFF) as usize;
        if resp_len == 0 {
            return Ok(None);
        }
        if resp_len > MAX_IPC_RESPONSE {
            return Err(AIOSException::Generic(format!(
                "Block '{}' returned oversized response ({resp_len} bytes)",
                block.name()
            )));
        }

        let resp_ptr = (packed >> 32) as usize;
        let bytes = block.read_linear_memory(store, resp_ptr, resp_len)?;
        Ok(Some(bytes))
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

/// Write `request` into the guest's linear memory and return the offset.
///
/// Prefers the guest's own `aios_alloc` export; otherwise the request is staged
/// in host-grown memory (`StoreState::ipc_scratch`), reusing the same buffer
/// across calls and never overlapping the block's own initial memory.
fn stage_request(
    block: &WasmBlock,
    store: &mut Store<StoreState>,
    request: &[u8],
) -> Result<usize> {
    if block.has_func(store, IPC_ALLOC_EXPORT) {
        let results = block.call_func(
            store,
            IPC_ALLOC_EXPORT,
            &[wasmtime::Val::I32(request.len() as i32)],
        )?;
        let ptr = results.first().and_then(|v| v.i32()).unwrap_or(0);
        if ptr <= 0 {
            return Err(AIOSException::Generic(format!(
                "Block '{}' allocator returned no space for {} bytes",
                block.name(),
                request.len()
            )));
        }
        let ptr = ptr as usize;
        block.write_linear_memory(store, ptr, request)?;
        return Ok(ptr);
    }

    if request.len() > MAX_IPC_REQUEST {
        return Err(AIOSException::Generic(format!(
            "Block '{}' IPC request ({} bytes) exceeds the {}-byte limit",
            block.name(),
            request.len(),
            MAX_IPC_REQUEST
        )));
    }

    let size = block.memory_size(store);
    if size == 0 {
        return Err(AIOSException::Generic(format!(
            "Block '{}' has no exported linear memory for IPC",
            block.name()
        )));
    }

    let mut base = store.data().ipc_scratch;
    if base == 0 || base + request.len() > size {
        // Place the buffer in freshly grown memory so it never overlaps the
        // block's own data, then remember it for subsequent calls.
        let needed = size + request.len();
        let pages = (needed - size).div_ceil(WASM_PAGE) as u64;
        block.grow_memory(store, pages)?;
        let grown = block.memory_size(store);
        base = grown - request.len();
        store.data_mut().ipc_scratch = base;
    }

    block.write_linear_memory(store, base, request)?;
    Ok(base)
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

    /// A block that echoes the staged request back as its response, exercising
    /// the full host→guest→host round trip without a guest allocator.
    const IPC_ECHO_WAT: &str = r#"
        (module
            (memory (export "memory") 1)
            (func (export "handle_message") (param i32 i32) (result i64)
                local.get 0
                i64.extend_i32_u
                i64.const 32
                i64.shl
                local.get 1
                i64.extend_i32_u
                i64.or)
        )
    "#;

    #[test]
    fn test_dispatch_wasm_packet_round_trip() {
        use aios_core::ipc_protocol::{CommandId, Payload};

        let mut registry = BlockRegistry::new();
        let manifest = BlockLoader::load_from_binary(
            &mut registry,
            "echo",
            "1.0.0",
            IPC_ECHO_WAT.as_bytes().to_vec(),
        )
        .unwrap();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();

        assert!(executor.routes_ipc(manifest.id));

        let packet = IpcPacket::new(
            1,
            2,
            CommandId::Custom,
            Payload::Custom("ping".into(), vec![1, 2, 3]),
        );
        let response = executor
            .dispatch_wasm_packet(manifest.id, &packet)
            .unwrap()
            .unwrap();
        assert_eq!(response, packet);
    }

    #[test]
    fn test_dispatch_wasm_no_response() {
        use aios_core::ipc_protocol::{CommandId, Payload};

        let wat = r#"
            (module
                (memory (export "memory") 1)
                (func (export "handle_message") (param i32 i32) (result i64)
                    i64.const 0)
            )
        "#;
        let mut registry = BlockRegistry::new();
        let manifest = BlockLoader::load_from_binary(
            &mut registry,
            "silent",
            "1.0.0",
            wat.as_bytes().to_vec(),
        )
        .unwrap();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();

        let packet = IpcPacket::new(0, 1, CommandId::Custom, Payload::Custom("x".into(), vec![]));
        assert!(executor
            .dispatch_wasm_packet(manifest.id, &packet)
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_dispatch_wasm_without_entry_fails() {
        use aios_core::ipc_protocol::{CommandId, Payload};

        let mut registry = BlockRegistry::new();
        let manifest =
            BlockLoader::load_from_binary(&mut registry, "no_entry", "1.0.0", sample_wasm_binary())
                .unwrap();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();

        assert!(!executor.routes_ipc(manifest.id));

        let packet = IpcPacket::new(0, 1, CommandId::Custom, Payload::Custom("x".into(), vec![]));
        assert!(executor.dispatch_wasm_packet(manifest.id, &packet).is_err());
    }

    #[test]
    fn test_dispatch_wasm_grows_memory_for_large_request() {
        use aios_core::ipc_protocol::{CommandId, Payload};

        // A block with only the minimum (1 page) of memory still receives a
        // request larger than a page thanks to host-side memory growth.
        let wat = r#"
            (module
                (memory (export "memory") 1)
                (func (export "handle_message") (param i32 i32) (result i64)
                    local.get 0
                    i64.extend_i32_u
                    i64.const 32
                    i64.shl
                    local.get 1
                    i64.extend_i32_u
                    i64.or)
            )
        "#;
        let mut registry = BlockRegistry::new();
        let manifest = BlockLoader::load_from_binary(
            &mut registry,
            "grower",
            "1.0.0",
            wat.as_bytes().to_vec(),
        )
        .unwrap();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        executor
            .execute_block(&registry, manifest.id, IsolationConfig::default())
            .unwrap();

        let blob = vec![7u8; 200 * 1024];
        let packet = IpcPacket::new(0, 1, CommandId::Custom, Payload::Custom("big".into(), blob));
        let response = executor
            .dispatch_wasm_packet(manifest.id, &packet)
            .unwrap()
            .unwrap();
        assert_eq!(response, packet);
    }
}
