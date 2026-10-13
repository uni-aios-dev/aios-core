//! Uniform block loader for the kernel.
//!
//! AIOS is block-oriented end to end: every unit above the scheduler is a
//! *block*, and the only difference between a **base** block and a **disk**
//! block is where the code comes from.
//!
//! * Base (native) blocks are compiled into the binary and produced on demand
//!   by a [`BlockFactory`] registered in [`base_factories`]. They are the
//!   guaranteed half of the hybrid model.
//! * Disk blocks are WebAssembly modules discovered under `AIOS_BLOCKS_DIR`
//!   (populated by the installer) and instantiated by the [`BlockExecutor`].
//!   Only the blocks that were actually dropped in by the installer run, so
//!   boot time stays proportional to the installed set.
//!
//! Both kinds end up registered in the same [`BlockRegistry`] and are
//! serviced over the same [`MessageRouter`], keeping the loader uniform.

use aios_block_mgr::factory::{BlockFactory, BlockFactoryRegistry};
use aios_block_mgr::registry::BlockRegistry;
use aios_block_mgr::router::MessageRouter;
use aios_browser::block::BrowserBlock;
use aios_browser::types::BrowserConfig;
use aios_core::block::{BlockId, StatefulBlock};
use aios_core::error::{AIOSException, Result};
use aios_hal::hardware::HalBlock;
use aios_net_config::block::NetSettingsBlock;
use aios_net_config::config::NetworkConfig;
use aios_wasm::executor::BlockExecutor;
use aios_wasm::isolation::IsolationConfig;
use std::path::Path;

/// WebAssembly magic number (`\0asm`), used to tell a real module apart from a
/// registry-only placeholder or a native `.bin` payload.
const WASM_MAGIC: &[u8] = b"\0asm";

/// A compiled-in base block that was wired into the router.
#[derive(Debug, Clone)]
pub struct BaseBlockInfo {
    /// Registry id assigned to the block.
    pub id: BlockId,
    /// Block name (also the factory lookup key).
    pub name: String,
    /// Block version.
    pub version: String,
}

/// A disk block that was discovered and executed.
#[derive(Debug, Clone)]
pub struct DiskBlockInfo {
    /// Registry id assigned to the block.
    pub id: BlockId,
    /// Block name derived from the file stem.
    pub name: String,
    /// Block version derived from the file stem.
    pub version: String,
    /// Exported functions the executor called during bring-up.
    pub functions_called: Vec<String>,
}

/// Aggregate result of the on-disk block phase.
#[derive(Debug, Default, Clone)]
pub struct DiskBlockReport {
    /// Blocks that were executed successfully.
    pub loaded: Vec<DiskBlockInfo>,
    /// Blocks that were registered but carry a non-WASM payload (`.bin`).
    pub registered_only: Vec<String>,
    /// Blocks that failed discovery or execution, as `(name, error)`.
    pub failed: Vec<(String, String)>,
}

impl DiskBlockReport {
    /// Number of executed disk blocks.
    pub fn loaded_count(&self) -> usize {
        self.loaded.len()
    }

    /// Number of blocks that failed to load.
    pub fn failed_count(&self) -> usize {
        self.failed.len()
    }
}

/// Factory for the `hal` base block.
struct HalFactory;

impl BlockFactory for HalFactory {
    fn name(&self) -> &str {
        "hal"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn create(&self, id: BlockId) -> Result<Box<dyn StatefulBlock>> {
        Ok(Box::new(HalBlock::new(id)))
    }
}

/// Factory for the `browser` base block.
struct BrowserFactory;

impl BlockFactory for BrowserFactory {
    fn name(&self) -> &str {
        "browser"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn create(&self, id: BlockId) -> Result<Box<dyn StatefulBlock>> {
        Ok(Box::new(BrowserBlock::new(id, BrowserConfig::default())))
    }
}

/// Factory for the `net_settings` base block.
struct NetSettingsFactory;

impl BlockFactory for NetSettingsFactory {
    fn name(&self) -> &str {
        "net_settings"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn create(&self, id: BlockId) -> Result<Box<dyn StatefulBlock>> {
        Ok(Box::new(NetSettingsBlock::with_default_store(
            id,
            NetworkConfig::default(),
        )))
    }
}

/// Build the registry of compiled-in base block factories.
///
/// The order is the registration order and is stable across builds. Adding a
/// block to the base set only requires a new factory here (and, once the
/// installer grows a catalog, a matching entry in `base-*` feature flags).
pub fn base_factories() -> BlockFactoryRegistry {
    let mut factories = BlockFactoryRegistry::new();
    // Names are unique constants, so registration cannot fail here.
    let _ = factories.register(Box::new(HalFactory));
    let _ = factories.register(Box::new(BrowserFactory));
    let _ = factories.register(Box::new(NetSettingsFactory));
    factories
}

/// Instantiate every base block, register it in the [`BlockRegistry`] and wire
/// its message handler into the [`MessageRouter`].
///
/// Returns the blocks that were wired. A block that fails to build is logged
/// and skipped so one bad factory cannot abort the whole boot.
pub fn register_base_blocks(
    registry: &mut BlockRegistry,
    router: &mut MessageRouter,
) -> Vec<BaseBlockInfo> {
    let factories = base_factories();
    let mut wired = Vec::new();
    for name in factories.names() {
        match register_one_base(&factories, name, registry, router) {
            Ok(info) => wired.push(info),
            Err(e) => log::error!("AIOS ERROR: base block '{name}' failed to wire: {e}"),
        }
    }
    wired
}

fn register_one_base(
    factories: &BlockFactoryRegistry,
    name: &str,
    registry: &mut BlockRegistry,
    router: &mut MessageRouter,
) -> Result<BaseBlockInfo> {
    let factory = factories
        .get(name)
        .ok_or_else(|| AIOSException::BlockNotFound(name.to_string()))?;

    let marker = format!("native:{}:{}", factory.name(), factory.version()).into_bytes();
    let id = registry.register_block(factory.name(), factory.version(), marker)?;
    registry.activate_block(id)?;

    let mut block = factory.create(id)?;
    router.register_handler(id.0, Box::new(move |packet| block.handle_message(packet)));

    log::info!(
        "AIOS: base block '{}' ({}) wired v{}",
        factory.name(),
        id,
        factory.version()
    );

    Ok(BaseBlockInfo {
        id,
        name: factory.name().to_string(),
        version: factory.version().to_string(),
    })
}

/// Whether `binary` starts with the WebAssembly magic number.
pub fn looks_like_wasm(binary: &[u8]) -> bool {
    binary.len() >= WASM_MAGIC.len() && &binary[..WASM_MAGIC.len()] == WASM_MAGIC
}

/// Discover and execute every on-disk block under `dir`.
///
/// Discovery registers each `.wasm`/`.bin` file (and its sidecar metadata) in
/// the registry; any payload that is a real WebAssembly module is then
/// instantiated block by block through the [`BlockExecutor`]. Placeholder or
/// native `.bin` payloads are registered only. Failures are collected rather
/// than propagated so a single broken block cannot stall the boot.
pub fn load_disk_blocks(
    registry: &mut BlockRegistry,
    executor: &mut BlockExecutor,
    dir: &Path,
) -> DiskBlockReport {
    let discoveries = registry.boot_discover(dir);
    let mut report = DiskBlockReport::default();

    for discovery in discoveries {
        let manifest = match discovery {
            Ok(manifest) => manifest,
            Err(e) => {
                report
                    .failed
                    .push(("<discovery>".to_string(), e.to_string()));
                continue;
            }
        };

        let binary = match registry.get(manifest.id) {
            Ok(entry) => entry.binary.clone(),
            Err(e) => {
                report.failed.push((manifest.name.clone(), e.to_string()));
                continue;
            }
        };

        if !looks_like_wasm(&binary) {
            log::debug!(
                "AIOS: disk block '{}' is not WebAssembly — registered only",
                manifest.name
            );
            report.registered_only.push(manifest.name);
            continue;
        }

        match executor.execute_block(registry, manifest.id, IsolationConfig::default()) {
            Ok(result) => report.loaded.push(DiskBlockInfo {
                id: manifest.id,
                name: result.name,
                version: result.version,
                functions_called: result.functions_called,
            }),
            Err(e) => {
                log::error!(
                    "AIOS ERROR: disk block '{}' failed to execute: {e}",
                    manifest.name
                );
                report.failed.push((manifest.name, e.to_string()));
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};

    #[test]
    fn test_base_factories_exposes_expected_blocks() {
        let factories = base_factories();
        assert_eq!(factories.names(), vec!["hal", "browser", "net_settings"]);
        assert!(factories.contains("net_settings"));
        assert!(!factories.contains("nonexistent"));
    }

    #[test]
    fn test_register_base_blocks_wires_registry_and_router() {
        let mut registry = BlockRegistry::new();
        let mut router = MessageRouter::new();

        let wired = register_base_blocks(&mut registry, &mut router);
        assert_eq!(wired.len(), 3);

        let names: Vec<&str> = wired.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"hal"));
        assert!(names.contains(&"browser"));
        assert!(names.contains(&"net_settings"));

        let net_id = registry.find_by_name("net_settings").unwrap().manifest.id;

        let packet = IpcPacket::new(
            0,
            net_id.0,
            CommandId::Custom,
            Payload::Custom("net_get".into(), Vec::new()),
        );
        let resp = router.dispatch(&packet).unwrap().unwrap();
        let json = match resp.payload {
            Payload::Text(t) => t,
            other => panic!("expected text response, got {other:?}"),
        };
        let config = NetworkConfig::from_json(&json).unwrap();
        assert_eq!(config.hostname, "aios-host");
    }

    #[test]
    fn test_looks_like_wasm() {
        assert!(looks_like_wasm(b"\0asm\x01\0\0\0"));
        assert!(!looks_like_wasm(b"native:hal:1.0.0"));
        assert!(!looks_like_wasm(b"\0as"));
        assert!(!looks_like_wasm(b""));
    }

    #[test]
    fn test_load_disk_blocks_executes_wasm_and_skips_bin() {
        // Minimal valid WebAssembly: two exported no-arg functions `init` and
        // `start`, matching what a real disk block provides.
        const DEMO_WASM: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic + version
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type: () -> ()
            0x03, 0x03, 0x02, 0x00, 0x00, // funcs 0,1 : type 0
            0x07, 0x10, 0x02, 0x04, b'i', b'n', b'i', b't', 0x00, 0x00, // export "init"
            0x05, b's', b't', b'a', b'r', b't', 0x00, 0x01, // export "start"
            0x0a, 0x07, 0x02, 0x02, 0x00, 0x0b, 0x02, 0x00, 0x0b, // code: empty bodies
        ];

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("demo_1.0.0.wasm"), DEMO_WASM).unwrap();
        std::fs::write(dir.path().join("legacy_1.0.0.bin"), b"not wasm").unwrap();

        let mut registry = BlockRegistry::new();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        let report = load_disk_blocks(&mut registry, &mut executor, dir.path());

        assert_eq!(report.loaded_count(), 1);
        assert_eq!(report.failed_count(), 0);
        assert!(report.loaded[0]
            .functions_called
            .contains(&"init".to_string()));
        assert!(report.loaded[0]
            .functions_called
            .contains(&"start".to_string()));
        assert!(report.registered_only.contains(&"legacy".to_string()));
        assert!(registry.find_by_name("demo").is_some());
        assert!(executor.executed_count() >= 1);
    }

    #[test]
    fn test_load_disk_blocks_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = BlockRegistry::new();
        let mut executor = BlockExecutor::with_default_config().unwrap();
        let report = load_disk_blocks(&mut registry, &mut executor, dir.path());

        assert_eq!(report.loaded_count(), 0);
        assert_eq!(report.failed_count(), 0);
    }
}
