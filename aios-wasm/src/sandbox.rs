use crate::isolation::IsolationConfig;
use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wasmtime::*;

pub const AIOS_MODULE: &str = "aios";

/// Number of epoch ticks a store is allowed to run before the ticker's
/// `timeout_ms` window elapses. The ticker fires every `timeout_ms / 4`.
pub const EPOCH_TICKS_PER_TIMEOUT: u64 = 4;

/// Background thread that increments the engine epoch once per tick window.
/// A store armed with [`Store::set_epoch_deadline`] is interrupted as soon as
/// the epoch has been incremented `EPOCH_TICKS_PER_TIMEOUT` times, which bounds
/// every wasm call to roughly `timeout_ms` of wall-clock time regardless of the
/// fuel limit.
struct EpochTicker {
    state: Arc<EpochTickerState>,
    handle: Option<JoinHandle<()>>,
}

struct EpochTickerState {
    stop: AtomicBool,
    cv: Condvar,
    mtx: Mutex<()>,
}

impl EpochTicker {
    fn start(engine: &Engine, timeout_ms: u64) -> Self {
        let state = Arc::new(EpochTickerState {
            stop: AtomicBool::new(false),
            cv: Condvar::new(),
            mtx: Mutex::new(()),
        });
        let ticker_state = state.clone();
        let engine = engine.clone();
        let tick_ms = (timeout_ms / EPOCH_TICKS_PER_TIMEOUT).max(1);

        let handle = std::thread::Builder::new()
            .name("aios-wasm-epoch-ticker".to_string())
            .spawn(move || {
                let mut guard = ticker_state
                    .mtx
                    .lock()
                    .expect("epoch ticker mutex poisoned");
                loop {
                    if ticker_state.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let (g, _) = ticker_state
                        .cv
                        .wait_timeout(guard, Duration::from_millis(tick_ms))
                        .expect("epoch ticker condvar poisoned");
                    guard = g;
                    if ticker_state.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    engine.increment_epoch();
                }
            })
            .map_err(|e| {
                log::warn!("WASM: failed to start epoch ticker thread: {e}");
            })
            .ok();

        Self { state, handle }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        self.state.cv.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub memory_limit_pages: u32,
    pub fuel_limit: u64,
    pub max_instances: u32,
    /// Wall-clock budget per wasm call. Enforced by an epoch ticker thread that
    /// calls [`Engine::increment_epoch`] and per-call re-arming of the store
    /// deadline in [`WasmBlock::call_func`] and [`WasmBlock::instantiate`];
    /// the fuel limit additionally bounds total execution.
    pub timeout_ms: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            memory_limit_pages: 256,
            fuel_limit: 1_000_000_000,
            max_instances: 64,
            timeout_ms: 30_000,
        }
    }
}

pub struct WasmSandbox {
    engine: Engine,
    config: SandboxConfig,
    _ticker: EpochTicker,
}

impl WasmSandbox {
    pub fn new(config: SandboxConfig) -> Result<Self> {
        let mut engine_config = Config::new();
        engine_config.consume_fuel(true).epoch_interruption(true);

        let engine = Engine::new(&engine_config)
            .map_err(|e| AIOSException::Generic(format!("WASM engine init: {e}")))?;
        let ticker = EpochTicker::start(&engine, config.timeout_ms);

        log::info!(
            "WASM: Sandbox engine initialized (fuel={}, max_mem={} pages, timeout={}ms)",
            config.fuel_limit,
            config.memory_limit_pages,
            config.timeout_ms
        );

        Ok(Self {
            engine,
            config,
            _ticker: ticker,
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    pub fn compile_module(&self, wasm_bytes: &[u8]) -> Result<Module> {
        Module::from_binary(&self.engine, wasm_bytes)
            .or_else(|_| Module::new(&self.engine, wasm_bytes))
            .map_err(|e| AIOSException::Generic(format!("WASM compile: {e}")))
    }

    pub fn compile_any(&self, bytes: &[u8]) -> Result<Module> {
        Module::from_binary(&self.engine, bytes)
            .or_else(|_| Module::new(&self.engine, bytes))
            .or_else(|_| unsafe { Module::deserialize(&self.engine, bytes) })
            .map_err(|e| AIOSException::Generic(format!("WASM compile: {e}")))
    }

    pub fn compile_wat(&self, wat: &str) -> Result<Module> {
        Module::new(&self.engine, wat)
            .map_err(|e| AIOSException::Generic(format!("WASM wat compile: {e}")))
    }

    pub fn create_store(&self) -> Result<Store<StoreState>> {
        let mut store = Store::new(&self.engine, StoreState::default());
        store
            .set_fuel(self.config.fuel_limit)
            .map_err(|e| AIOSException::Generic(format!("WASM set fuel: {e}")))?;
        // wasmtime's default epoch deadline is 0 (immediate trap), so every
        // store is armed with a fresh timeout window. Each wasm call re-arms
        // the deadline via `arm_timeout` so long-lived stores do not trap once
        // their first window elapses.
        store.set_epoch_deadline(EPOCH_TICKS_PER_TIMEOUT);
        Ok(store)
    }

    pub fn arm_timeout(&self, store: &mut Store<StoreState>) {
        store.set_epoch_deadline(EPOCH_TICKS_PER_TIMEOUT);
    }
}

#[derive(Debug, Default)]
pub struct StoreState {
    pub memory_used_bytes: usize,
    pub fuel_consumed: u64,
    pub blocks_loaded: u32,
}

pub struct WasmBlock {
    name: String,
    version: String,
    engine: Engine,
    module: Module,
    instance: Option<Instance>,
    config: SandboxConfig,
    isolation: IsolationConfig,
    _ticker: EpochTicker,
}

impl WasmBlock {
    pub fn new(
        name: String,
        version: String,
        wasm_bytes: &[u8],
        config: SandboxConfig,
        isolation: IsolationConfig,
    ) -> Result<Self> {
        let sandbox = WasmSandbox::new(config.clone())?;
        let engine = sandbox.engine().clone();
        let module = sandbox.compile_any(wasm_bytes)?;
        let ticker = EpochTicker::start(&engine, config.timeout_ms);

        log::info!(
            "WASM: Block loaded — {} v{} ({} bytes, memory={} pages, fuel={}, timeout={}ms)",
            name,
            version,
            wasm_bytes.len(),
            config.memory_limit_pages,
            config.fuel_limit,
            config.timeout_ms
        );

        Ok(Self {
            name,
            version,
            engine,
            module,
            instance: None,
            config,
            isolation,
            _ticker: ticker,
        })
    }

    pub fn from_wat(
        name: String,
        version: String,
        wat: &str,
        config: SandboxConfig,
        isolation: IsolationConfig,
    ) -> Result<Self> {
        let sandbox = WasmSandbox::new(config.clone())?;
        let engine = sandbox.engine().clone();
        let module = sandbox.compile_wat(wat)?;
        let ticker = EpochTicker::start(&engine, config.timeout_ms);

        log::info!("WASM: Block loaded from WAT — {} v{}", name, version);

        Ok(Self {
            name,
            version,
            engine,
            module,
            instance: None,
            config,
            isolation,
            _ticker: ticker,
        })
    }

    pub fn instantiate(&mut self, store: &mut Store<StoreState>) -> Result<()> {
        let mut linker = Linker::new(store.engine());
        Self::register_aios_host_functions(&mut linker)?;

        self.arm_timeout(store);
        let needs_imports = self.module.imports().count() > 0;
        if !needs_imports {
            let instance = linker
                .instantiate(&mut *store, &self.module)
                .map_err(|e| AIOSException::Generic(format!("WASM instantiate: {e}")))?;
            self.instance = Some(instance);
        } else {
            let instance = linker.instantiate(&mut *store, &self.module).map_err(|e| {
                AIOSException::Generic(format!("WASM instantiate (with imports): {e}"))
            })?;
            self.instance = Some(instance);
        }

        store.data_mut().blocks_loaded += 1;
        log::info!(
            "WASM: Block {} v{} instantiated successfully",
            self.name,
            self.version
        );
        Ok(())
    }

    pub fn register_aios_host_functions(linker: &mut Linker<StoreState>) -> Result<()> {
        linker
            .func_wrap(
                AIOS_MODULE,
                "log",
                |mut caller: Caller<'_, StoreState>, ptr: i32, len: i32| -> i32 {
                    let memory = match caller.get_export("memory") {
                        Some(Extern::Memory(m)) => m,
                        _ => return -1,
                    };
                    let data = memory.data(caller.as_context());
                    let start = ptr.max(0) as usize;
                    let end = (start + len.max(0) as usize).min(data.len());
                    if start < end {
                        if let Ok(msg) = std::str::from_utf8(&data[start..end]) {
                            log::info!("[WASM:aios] {msg}");
                        }
                    }
                    0
                },
            )
            .map_err(|e| AIOSException::Generic(format!("WASM linker aios.log: {e}")))?;

        linker
            .func_wrap(AIOS_MODULE, "get_timestamp", || -> i64 {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0)
            })
            .map_err(|e| AIOSException::Generic(format!("WASM linker aios.get_timestamp: {e}")))?;

        Ok(())
    }

    pub fn create_store(&self) -> Result<Store<StoreState>> {
        let mut store = Store::new(&self.engine, StoreState::default());
        store
            .set_fuel(self.config.fuel_limit)
            .map_err(|e| AIOSException::Generic(format!("WASM set fuel: {e}")))?;
        store.set_epoch_deadline(EPOCH_TICKS_PER_TIMEOUT);
        Ok(store)
    }

    pub fn arm_timeout(&self, store: &mut Store<StoreState>) {
        store.set_epoch_deadline(EPOCH_TICKS_PER_TIMEOUT);
    }

    pub fn call_func(
        &self,
        store: &mut Store<StoreState>,
        func_name: &str,
        args: &[Val],
    ) -> Result<Vec<Val>> {
        let instance = self.instance.as_ref().ok_or_else(|| {
            AIOSException::Generic(format!("Block {} not instantiated", self.name))
        })?;

        let func = instance.get_func(&mut *store, func_name).ok_or_else(|| {
            AIOSException::Generic(format!(
                "Function '{}' not found in block {}",
                func_name, self.name
            ))
        })?;

        self.arm_timeout(store);
        let mut results = vec![Val::I32(0); func.ty(&*store).results().len()];
        func.call(&mut *store, args, &mut results)
            .map_err(|e| AIOSException::Generic(format!("WASM call: {e}")))?;

        Ok(results)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    pub fn isolation(&self) -> &IsolationConfig {
        &self.isolation
    }

    pub fn is_instantiated(&self) -> bool {
        self.instance.is_some()
    }

    pub fn instance_ref(&self) -> Option<&Instance> {
        self.instance.as_ref()
    }

    pub fn memory_stats(&self) -> MemoryStats {
        MemoryStats {
            limit_pages: self.config.memory_limit_pages,
            fuel_limit: self.config.fuel_limit,
            is_instantiated: self.instance.is_some(),
        }
    }

    pub fn extract_linear_memory(&self, store: &mut Store<StoreState>) -> Option<Vec<u8>> {
        let instance = self.instance.as_ref()?;
        let memory = instance.get_memory(store.as_context_mut(), "memory")?;
        let data = memory.data(store.as_context());
        if data.is_empty() {
            None
        } else {
            Some(data.to_vec())
        }
    }

    pub fn restore_linear_memory(&self, store: &mut Store<StoreState>, data: &[u8]) -> bool {
        let instance = match self.instance.as_ref() {
            Some(i) => i,
            None => return false,
        };
        let memory = match instance.get_memory(store.as_context_mut(), "memory") {
            Some(m) => m,
            None => return false,
        };
        let current_len = memory.data(store.as_context()).len();
        if data.len() > current_len {
            log::warn!(
                "WASM: Cannot restore {} bytes into {} bytes of linear memory for {}",
                data.len(),
                current_len,
                self.name
            );
            return false;
        }
        let dest = memory.data_mut(store.as_context_mut());
        dest[..data.len()].copy_from_slice(data);
        log::info!(
            "WASM: Restored {} bytes of linear memory for {}",
            data.len(),
            self.name
        );
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub limit_pages: u32,
    pub fuel_limit: u64,
    pub is_instantiated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isolation::IsolationConfig;

    const SIMPLE_WAT: &str = r#"
        (module
            (func (export "add") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add)
            (func (export "greet") (result i32)
                i32.const 42)
        )
    "#;

    #[test]
    fn test_sandbox_creation() {
        let config = SandboxConfig::default();
        let sandbox = WasmSandbox::new(config);
        assert!(sandbox.is_ok());
    }

    #[test]
    fn test_compile_wat() {
        let sandbox = WasmSandbox::new(SandboxConfig::default()).unwrap();
        let module = sandbox.compile_wat(SIMPLE_WAT);
        assert!(module.is_ok());
    }

    #[test]
    fn test_store_creation() {
        let sandbox = WasmSandbox::new(SandboxConfig::default()).unwrap();
        let store = sandbox.create_store();
        assert!(store.is_ok());
    }

    #[test]
    fn test_wasm_block_from_wat() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let block = WasmBlock::from_wat(
            "test_block".into(),
            "1.0.0".into(),
            SIMPLE_WAT,
            config,
            isolation,
        );
        assert!(block.is_ok());
        let block = block.unwrap();
        assert_eq!(block.name(), "test_block");
        assert_eq!(block.version(), "1.0.0");
        assert!(!block.is_instantiated());
    }

    #[test]
    fn test_wasm_block_instantiate_and_call() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let mut block = WasmBlock::from_wat(
            "test_block".into(),
            "1.0.0".into(),
            SIMPLE_WAT,
            config,
            isolation,
        )
        .unwrap();

        let mut store = block.create_store().unwrap();

        block.instantiate(&mut store).unwrap();
        assert!(block.is_instantiated());

        let results = block
            .call_func(&mut store, "add", &[Val::I32(3), Val::I32(4)])
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].i32(), Some(7));
    }

    #[test]
    fn test_wasm_block_call_greet() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let mut block = WasmBlock::from_wat(
            "test_block".into(),
            "1.0.0".into(),
            SIMPLE_WAT,
            config,
            isolation,
        )
        .unwrap();

        let mut store = block.create_store().unwrap();

        block.instantiate(&mut store).unwrap();
        let results = block.call_func(&mut store, "greet", &[]).unwrap();
        assert_eq!(results[0].i32(), Some(42));
    }

    #[test]
    fn test_wasm_block_memory_stats() {
        let config = SandboxConfig {
            memory_limit_pages: 512,
            fuel_limit: 500_000_000,
            ..Default::default()
        };
        let isolation = IsolationConfig::default();
        let block =
            WasmBlock::from_wat("test".into(), "1.0.0".into(), SIMPLE_WAT, config, isolation)
                .unwrap();
        let stats = block.memory_stats();
        assert_eq!(stats.limit_pages, 512);
        assert_eq!(stats.fuel_limit, 500_000_000);
        assert!(!stats.is_instantiated);
    }

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert_eq!(config.memory_limit_pages, 256);
        assert_eq!(config.fuel_limit, 1_000_000_000);
        assert_eq!(config.max_instances, 64);
        assert_eq!(config.timeout_ms, 30_000);
    }

    #[test]
    fn test_wasm_block_not_instantiated_call_fails() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let block =
            WasmBlock::from_wat("test".into(), "1.0.0".into(), SIMPLE_WAT, config, isolation)
                .unwrap();
        let mut store = block.create_store().unwrap();
        let result = block.call_func(&mut store, "add", &[Val::I32(1), Val::I32(2)]);
        assert!(result.is_err());
    }

    #[test]
    fn test_wasm_block_missing_function_fails() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let mut block =
            WasmBlock::from_wat("test".into(), "1.0.0".into(), SIMPLE_WAT, config, isolation)
                .unwrap();
        let mut store = block.create_store().unwrap();
        block.instantiate(&mut store).unwrap();
        let result = block.call_func(&mut store, "nonexistent", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_sandbox_config_serialization() {
        let config = SandboxConfig::default();
        let bytes = bincode::serialize(&config).unwrap();
        let restored: SandboxConfig = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.memory_limit_pages, 256);
        assert_eq!(restored.fuel_limit, 1_000_000_000);
    }

    #[test]
    fn test_memory_stats_serialization() {
        let stats = MemoryStats {
            limit_pages: 512,
            fuel_limit: 999,
            is_instantiated: true,
        };
        let bytes = bincode::serialize(&stats).unwrap();
        let restored: MemoryStats = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.limit_pages, 512);
        assert!(restored.is_instantiated);
    }

    #[test]
    fn test_store_state_default() {
        let state = StoreState::default();
        assert_eq!(state.memory_used_bytes, 0);
        assert_eq!(state.fuel_consumed, 0);
        assert_eq!(state.blocks_loaded, 0);
    }

    #[test]
    fn test_store_tracks_blocks_loaded() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let mut block =
            WasmBlock::from_wat("test".into(), "1.0.0".into(), SIMPLE_WAT, config, isolation)
                .unwrap();

        let mut store = block.create_store().unwrap();
        assert_eq!(store.data().blocks_loaded, 0);

        block.instantiate(&mut store).unwrap();
        assert_eq!(store.data().blocks_loaded, 1);
    }

    const MEMORY_WAT: &str = r#"
        (module
            (memory (export "memory") 1 4)
            (func (export "init"))
            (func (export "start"))
            (func (export "write_val") (param i32)
                (i32.store (i32.const 0) (local.get 0)))
            (func (export "read_val") (result i32)
                (i32.load (i32.const 0)))
        )
    "#;

    #[test]
    fn test_extract_linear_memory_empty_initial() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let mut block =
            WasmBlock::from_wat("mem".into(), "1.0.0".into(), MEMORY_WAT, config, isolation)
                .unwrap();
        let mut store = block.create_store().unwrap();
        block.instantiate(&mut store).unwrap();

        let mem = block.extract_linear_memory(&mut store);
        assert!(mem.is_some());
        let data = mem.unwrap();
        assert!(!data.is_empty());
        assert!(data.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_restore_and_extract_linear_memory() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let mut block =
            WasmBlock::from_wat("mem".into(), "1.0.0".into(), MEMORY_WAT, config, isolation)
                .unwrap();
        let mut store = block.create_store().unwrap();
        block.instantiate(&mut store).unwrap();

        block
            .call_func(&mut store, "write_val", &[Val::I32(123)])
            .unwrap();
        let val = block.call_func(&mut store, "read_val", &[]).unwrap();
        assert_eq!(val[0].i32(), Some(123));

        let snapshot = block.extract_linear_memory(&mut store).unwrap();
        assert_eq!(snapshot[0], 123);

        let mut block2 = WasmBlock::from_wat(
            "mem2".into(),
            "1.0.0".into(),
            MEMORY_WAT,
            SandboxConfig::default(),
            IsolationConfig::default(),
        )
        .unwrap();
        let mut store2 = block2.create_store().unwrap();
        block2.instantiate(&mut store2).unwrap();

        block2.restore_linear_memory(&mut store2, &snapshot);
        let val2 = block2.call_func(&mut store2, "read_val", &[]).unwrap();
        assert_eq!(val2[0].i32(), Some(123));
    }

    #[test]
    fn test_extract_linear_memory_no_memory_block() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let mut block = WasmBlock::from_wat(
            "nomem".into(),
            "1.0.0".into(),
            SIMPLE_WAT,
            config,
            isolation,
        )
        .unwrap();
        let mut store = block.create_store().unwrap();
        block.instantiate(&mut store).unwrap();

        let mem = block.extract_linear_memory(&mut store);
        assert!(mem.is_none());
    }

    #[test]
    fn test_restore_linear_memory_not_instantiated() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let block = WasmBlock::from_wat(
            "nomem".into(),
            "1.0.0".into(),
            SIMPLE_WAT,
            config,
            isolation,
        )
        .unwrap();
        let mut store = block.create_store().unwrap();
        let restored = block.restore_linear_memory(&mut store, &[1, 2, 3]);
        assert!(!restored);
    }

    #[test]
    fn test_restore_linear_memory_rejects_oversized_data() {
        let config = SandboxConfig::default();
        let isolation = IsolationConfig::default();
        let mut block =
            WasmBlock::from_wat("mem".into(), "1.0.0".into(), MEMORY_WAT, config, isolation)
                .unwrap();
        let mut store = block.create_store().unwrap();
        block.instantiate(&mut store).unwrap();

        let oversized = vec![0u8; 65536 + 1];
        assert!(
            !block.restore_linear_memory(&mut store, &oversized),
            "restoring more bytes than the linear memory holds must fail, not truncate"
        );
    }

    #[test]
    fn test_epoch_timeout_interrupts_runaway_wasm() {
        const SPIN_WAT: &str = r#"
            (module
                (func (export "spin")
                    (loop $l
                        br $l)))
        "#;
        // Fuel high enough that exhaustion alone would take ~10 s; the epoch
        // ticker must interrupt the loop in ~timeout_ms instead.
        let config = SandboxConfig {
            timeout_ms: 150,
            fuel_limit: 10_000_000_000,
            ..SandboxConfig::default()
        };
        let isolation = IsolationConfig::default();
        let mut block =
            WasmBlock::from_wat("spin".into(), "1.0.0".into(), SPIN_WAT, config, isolation)
                .unwrap();
        let mut store = block.create_store().unwrap();
        block.instantiate(&mut store).unwrap();

        let start = std::time::Instant::now();
        let result = block.call_func(&mut store, "spin", &[]);
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "runaway wasm must be interrupted by the epoch timeout, not run until fuel runs out"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("wasm backtrace"),
            "expected a wasm-time trap, got: {err:?}"
        );
        assert!(
            elapsed.as_millis() < 5_000,
            "epoch interrupt must fire near the timeout, took {elapsed:?} (fuel-only trap takes ~10s)"
        );
    }

    #[test]
    fn test_epoch_deadline_rearmed_between_calls() {
        const COUNT_WAT: &str = r#"
            (module
                (func (export "count") (param i32) (result i32)
                    local.get 0
                    i32.const 1
                    i32.add))
        "#;
        let config = SandboxConfig {
            timeout_ms: 50,
            fuel_limit: 1_000_000_000,
            ..SandboxConfig::default()
        };
        let isolation = IsolationConfig::default();
        let mut block =
            WasmBlock::from_wat("count".into(), "1.0.0".into(), COUNT_WAT, config, isolation)
                .unwrap();
        let mut store = block.create_store().unwrap();
        block.instantiate(&mut store).unwrap();

        // Let several timeout windows elapse between calls; each call must still
        // run because `call_func` re-arms the epoch deadline.
        for i in 0..5 {
            std::thread::sleep(Duration::from_millis(60));
            let results = block
                .call_func(&mut store, "count", &[Val::I32(i)])
                .unwrap();
            assert_eq!(results[0].i32(), Some(i + 1));
        }
    }
}
