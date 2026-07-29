use aios_core::block::{BlockId, BlockManifest, BlockState};
use aios_core::error::{AIOSException, Result};
use aios_security::capability::CapabilityToken;
use std::collections::HashMap;

use crate::dependency::DependencyGraph;
use crate::loader::BlockLoader;
use std::path::Path;

pub struct BlockRegistry {
    blocks: HashMap<BlockId, BlockEntry>,
    next_id: u32,
    dependencies: HashMap<String, Vec<String>>,
}

pub struct BlockEntry {
    pub manifest: BlockManifest,
    pub state: BlockState,
    pub binary: Vec<u8>,
    pub capabilities: Option<CapabilityToken>,
}

impl BlockRegistry {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            next_id: 1,
            dependencies: HashMap::new(),
        }
    }

    pub fn register_block(
        &mut self,
        name: &str,
        version: &str,
        binary: Vec<u8>,
    ) -> Result<BlockId> {
        let id = BlockId::new(self.next_id);
        self.next_id += 1;

        let sha256 = aios_core::crypto::compute_sha256_bytes(&binary);
        let manifest = BlockManifest {
            id,
            name: name.to_string(),
            version: version.to_string(),
            sha256,
        };

        log::info!(
            "BlockManager: Registered '{}' ({}) from {} bytes",
            name,
            id,
            binary.len()
        );

        self.blocks.insert(
            id,
            BlockEntry {
                manifest,
                state: BlockState::Loaded,
                binary,
                capabilities: None,
            },
        );

        Ok(id)
    }

    pub fn assign_capabilities(&mut self, id: BlockId, token: CapabilityToken) -> Result<()> {
        let entry = self.get_mut(id)?;
        entry.capabilities = Some(token);
        Ok(())
    }

    pub fn check_capability(
        &self,
        id: BlockId,
        required: &aios_security::capability::Capability,
    ) -> Result<()> {
        let entry = self.get(id)?;
        match &entry.capabilities {
            Some(token) => {
                if token.is_expired() {
                    Err(AIOSException::PermissionDenied(format!(
                        "Token for block {} has expired",
                        id
                    )))
                } else if token.has_capability(required) {
                    Ok(())
                } else {
                    Err(AIOSException::PermissionDenied(format!(
                        "Block {} lacks capability {}",
                        id,
                        required.name()
                    )))
                }
            }
            None => Err(AIOSException::PermissionDenied(format!(
                "No capabilities assigned to block {}",
                id
            ))),
        }
    }

    pub fn activate_block(&mut self, id: BlockId) -> Result<()> {
        let entry = self.get_mut(id)?;
        entry.state = BlockState::Active;
        log::info!("BlockManager: Activated block {}", id);
        Ok(())
    }

    pub fn unload_block(&mut self, id: BlockId) -> Result<BlockEntry> {
        let entry = self
            .blocks
            .remove(&id)
            .ok_or_else(|| AIOSException::BlockNotFound(format!("{}", id)))?;
        log::info!(
            "BlockManager: Unloaded block {} ({})",
            entry.manifest.name,
            id
        );
        Ok(entry)
    }

    pub fn get(&self, id: BlockId) -> Result<&BlockEntry> {
        self.blocks
            .get(&id)
            .ok_or_else(|| AIOSException::BlockNotFound(format!("{}", id)))
    }

    pub fn get_mut(&mut self, id: BlockId) -> Result<&mut BlockEntry> {
        self.blocks
            .get_mut(&id)
            .ok_or_else(|| AIOSException::BlockNotFound(format!("{}", id)))
    }

    pub fn update_state(&mut self, id: BlockId, state: BlockState) -> Result<()> {
        let entry = self.get_mut(id)?;
        entry.state = state;
        Ok(())
    }

    pub fn topology(&self) -> Vec<BlockManifest> {
        self.blocks.values().map(|e| e.manifest.clone()).collect()
    }

    pub fn topology_with_state(&self) -> Vec<(BlockManifest, BlockState)> {
        self.blocks
            .values()
            .map(|e| (e.manifest.clone(), e.state))
            .collect()
    }

    pub fn count(&self) -> usize {
        self.blocks.len()
    }

    pub fn all_ids(&self) -> Vec<BlockId> {
        self.blocks.keys().copied().collect()
    }

    pub fn find_by_name(&self, name: &str) -> Option<&BlockEntry> {
        self.blocks.values().find(|e| e.manifest.name == name)
    }

    pub fn set_block_dependencies(&mut self, name: &str, deps: Vec<String>) {
        self.dependencies.insert(name.to_string(), deps);
    }

    pub fn dependency_graph(&self) -> DependencyGraph {
        let mut graph = DependencyGraph::new();
        for name in self.blocks.values().map(|e| &e.manifest.name) {
            graph.add_block(name);
        }
        for (name, deps) in &self.dependencies {
            for dep in deps {
                let _ = graph.add_dependency(name, dep);
            }
        }
        graph
    }

    pub fn verify_signature(&self, id: BlockId) -> Result<bool> {
        let entry = self.get(id)?;
        let actual = aios_core::crypto::compute_sha256_bytes(&entry.binary);
        Ok(actual == entry.manifest.sha256)
    }

    pub fn load_from_path(&mut self, dir: &Path) -> Vec<Result<BlockManifest>> {
        log::info!("BlockRegistry: Loading blocks from {:?}", dir);
        let results = BlockLoader::load_from_directory(self, dir);
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        let err_count = results.len() - ok_count;
        log::info!(
            "BlockRegistry: Loaded {} blocks ({} ok, {} failed) from {:?}",
            results.len(),
            ok_count,
            err_count,
            dir
        );
        results
    }

    pub fn load_from_path_str(&mut self, dir: &str) -> Vec<Result<BlockManifest>> {
        self.load_from_path(Path::new(dir))
    }

    pub fn boot_discover(&mut self, root: &Path) -> Vec<Result<BlockManifest>> {
        log::info!("BlockRegistry: Boot discovery from {:?}", root);
        let mut results = Vec::new();

        if !root.exists() {
            if let Err(e) = std::fs::create_dir_all(root) {
                log::warn!(
                    "BlockRegistry: Could not create blocks dir {:?}: {}",
                    root,
                    e
                );
                return results;
            }
            log::info!("BlockRegistry: Created blocks directory {:?}", root);
            return results;
        }

        self.walk_recursive(root, &mut results);

        let ok = results.iter().filter(|r| r.is_ok()).count();
        let err = results.len() - ok;
        log::info!(
            "BlockRegistry: Boot discovery complete — {} blocks ({} ok, {} failed)",
            results.len(),
            ok,
            err
        );
        results
    }

    fn walk_recursive(&mut self, dir: &Path, results: &mut Vec<Result<BlockManifest>>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                log::error!("BlockRegistry: failed to read {:?}: {}", dir, e);
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.walk_recursive(&path, results);
                continue;
            }

            let ext = path.extension().and_then(|e| e.to_str());
            if ext != Some("bin") && ext != Some("wasm") {
                continue;
            }

            let file_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            let parts: Vec<&str> = file_stem.splitn(2, '_').collect();
            let name = parts[0].to_string();
            let version = if parts.len() > 1 {
                parts[1].to_string()
            } else {
                "0.0.0".to_string()
            };

            match std::fs::read(&path) {
                Ok(binary) => {
                    log::info!(
                        "BlockRegistry: discovered block '{}' v{} from {:?}",
                        name,
                        version,
                        path
                    );
                    let result = self.register_block(&name, &version, binary);
                    match result {
                        Ok(block_id) => {
                            let entry = self.get(block_id).unwrap();
                            results.push(Ok(entry.manifest.clone()));
                        }
                        Err(e) => {
                            results.push(Err(e));
                        }
                    }
                }
                Err(e) => {
                    log::error!("BlockRegistry: failed to read {:?}: {}", path, e);
                    results.push(Err(AIOSException::IPCError(format!(
                        "Failed to read {:?}: {}",
                        path, e
                    ))));
                }
            }
        }
    }
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_binary(name: &str) -> Vec<u8> {
        format!("binary_data_for_{name}").into_bytes()
    }

    #[test]
    fn test_register_and_get() {
        let mut reg = BlockRegistry::new();
        let id = reg
            .register_block("test", "0.1.0", sample_binary("test"))
            .unwrap();
        let entry = reg.get(id).unwrap();
        assert_eq!(entry.manifest.name, "test");
        assert_eq!(entry.state, BlockState::Loaded);
    }

    #[test]
    fn test_unregister() {
        let mut reg = BlockRegistry::new();
        let id = reg
            .register_block("test", "0.1.0", sample_binary("test"))
            .unwrap();
        let removed = reg.unload_block(id).unwrap();
        assert_eq!(removed.manifest.name, "test");
        assert!(reg.get(id).is_err());
    }

    #[test]
    fn test_topology() {
        let mut reg = BlockRegistry::new();
        reg.register_block("a", "0.1.0", sample_binary("a"))
            .unwrap();
        reg.register_block("b", "0.2.0", sample_binary("b"))
            .unwrap();
        let topo = reg.topology();
        assert_eq!(topo.len(), 2);
    }

    #[test]
    fn test_activate_block() {
        let mut reg = BlockRegistry::new();
        let id = reg
            .register_block("test", "0.1.0", sample_binary("test"))
            .unwrap();
        reg.activate_block(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, BlockState::Active);
    }

    #[test]
    fn test_verify_signature() {
        let mut reg = BlockRegistry::new();
        let id = reg
            .register_block("test", "0.1.0", sample_binary("test"))
            .unwrap();
        assert!(reg.verify_signature(id).unwrap());
    }

    #[test]
    fn test_find_by_name() {
        let mut reg = BlockRegistry::new();
        reg.register_block("alpha", "0.1.0", sample_binary("alpha"))
            .unwrap();
        assert!(reg.find_by_name("alpha").is_some());
        assert!(reg.find_by_name("beta").is_none());
    }

    #[test]
    fn test_count() {
        let mut reg = BlockRegistry::new();
        assert_eq!(reg.count(), 0);
        reg.register_block("a", "0.1.0", sample_binary("a"))
            .unwrap();
        reg.register_block("b", "0.1.0", sample_binary("b"))
            .unwrap();
        assert_eq!(reg.count(), 2);
    }

    #[test]
    fn test_assign_and_check_capabilities() {
        use aios_security::capability::{Capability, CapabilityToken};

        let mut reg = BlockRegistry::new();
        let id = reg
            .register_block("secure", "1.0.0", sample_binary("secure"))
            .unwrap();

        assert!(reg.check_capability(id, &Capability::FsRead).is_err());

        let token = CapabilityToken::new(
            id.0,
            vec![Capability::FsRead, Capability::FsWrite],
            60_000,
            b"test_secret",
        );
        reg.assign_capabilities(id, token).unwrap();

        assert!(reg.check_capability(id, &Capability::FsRead).is_ok());
        assert!(reg.check_capability(id, &Capability::FsWrite).is_ok());
        assert!(reg.check_capability(id, &Capability::ProcessSpawn).is_err());
    }

    #[test]
    fn test_capabilities_none_by_default() {
        let mut reg = BlockRegistry::new();
        let id = reg
            .register_block("plain", "1.0.0", sample_binary("plain"))
            .unwrap();

        assert!(reg.get(id).unwrap().capabilities.is_none());
    }

    #[test]
    fn test_dependency_graph() {
        let mut reg = BlockRegistry::new();
        reg.register_block("hal", "1.0.0", sample_binary("hal"))
            .unwrap();
        reg.register_block("ipc_bus", "1.0.0", sample_binary("ipc_bus"))
            .unwrap();
        reg.register_block("scheduler", "1.0.0", sample_binary("scheduler"))
            .unwrap();

        reg.set_block_dependencies("ipc_bus", vec!["hal".into()]);
        reg.set_block_dependencies("scheduler", vec!["ipc_bus".into()]);

        let graph = reg.dependency_graph();
        let blocks: Vec<&str> = graph.blocks();
        assert!(blocks.contains(&"hal"));
        assert!(blocks.contains(&"ipc_bus"));
        assert!(blocks.contains(&"scheduler"));

        let deps = graph.dependencies_of("scheduler");
        assert_eq!(deps, vec!["ipc_bus"]);

        let deps = graph.dependencies_of("ipc_bus");
        assert_eq!(deps, vec!["hal"]);

        let deps = graph.dependencies_of("hal");
        assert!(deps.is_empty());

        let order = graph.load_order().unwrap();
        assert_eq!(order, vec!["hal", "ipc_bus", "scheduler"]);
    }

    #[test]
    fn test_load_from_path_with_wasm_files() {
        let dir = tempfile::tempdir().unwrap();
        let wasm_dir = dir.path();

        let wasm1 = r#"
            (module
                (func (export "init"))
                (func (export "start"))
            )
        "#
        .as_bytes();
        std::fs::write(wasm_dir.join("math_1.0.0.wasm"), wasm1).unwrap();

        let wasm2 = r#"
            (module
                (func (export "init"))
            )
        "#
        .as_bytes();
        std::fs::write(wasm_dir.join("crypto_2.0.0.wasm"), wasm2).unwrap();

        let mut reg = BlockRegistry::new();
        let results = reg.load_from_path(wasm_dir);

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));
        assert_eq!(reg.count(), 2);

        let math = reg.find_by_name("math").unwrap();
        assert_eq!(math.manifest.version, "1.0.0");
        assert_eq!(math.state, BlockState::Active);

        let crypto = reg.find_by_name("crypto").unwrap();
        assert_eq!(crypto.manifest.version, "2.0.0");
    }

    #[test]
    fn test_load_from_path_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = BlockRegistry::new();
        let results = reg.load_from_path(dir.path());
        assert!(results.is_empty());
        assert_eq!(reg.count(), 0);
    }

    #[test]
    fn test_load_from_path_nonexistent_dir() {
        let mut reg = BlockRegistry::new();
        let results = reg.load_from_path(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(results.is_empty());
        assert_eq!(reg.count(), 0);
    }

    #[test]
    fn test_load_from_path_mixed_bin_and_wasm() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("alpha_0.1.0.bin"), b"binary_alpha").unwrap();

        let wasm = r#"
            (module (func (export "init")))
        "#
        .as_bytes();
        std::fs::write(dir.path().join("beta_1.0.0.wasm"), wasm).unwrap();

        std::fs::write(dir.path().join("readme.txt"), b"not a block").unwrap();

        let mut reg = BlockRegistry::new();
        let results = reg.load_from_path(dir.path());

        assert_eq!(results.len(), 2);
        assert_eq!(reg.count(), 2);
        assert!(reg.find_by_name("alpha").is_some());
        assert!(reg.find_by_name("beta").is_some());
    }

    #[test]
    fn test_load_from_path_str() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = r#"
            (module (func (export "init")))
        "#
        .as_bytes();
        std::fs::write(dir.path().join("test_1.0.0.wasm"), wasm).unwrap();

        let mut reg = BlockRegistry::new();
        let path_str = dir.path().to_str().unwrap();
        let results = reg.load_from_path_str(path_str);

        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
        assert_eq!(reg.count(), 1);
    }

    #[test]
    fn test_boot_discover_creates_dir_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nonexistent_subdir");
        let mut reg = BlockRegistry::new();
        let results = reg.boot_discover(&missing);
        assert!(results.is_empty());
        assert!(missing.exists());
    }

    #[test]
    fn test_boot_discover_walks_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested").join("deep");
        std::fs::create_dir_all(&sub).unwrap();

        let wasm = r#"
            (module (func (export "init")))
        "#
        .as_bytes();
        std::fs::write(sub.join("subblock_1.0.0.wasm"), wasm).unwrap();
        std::fs::write(dir.path().join("topblock_2.0.0.wasm"), wasm).unwrap();

        let mut reg = BlockRegistry::new();
        let results = reg.boot_discover(dir.path());

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));
        assert_eq!(reg.count(), 2);
        assert!(reg.find_by_name("subblock").is_some());
        assert!(reg.find_by_name("topblock").is_some());
    }

    #[test]
    fn test_boot_discover_skips_non_block_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"not a block").unwrap();
        std::fs::write(dir.path().join("notes.md"), b"# notes").unwrap();

        let wasm = r#"
            (module (func (export "init")))
        "#
        .as_bytes();
        std::fs::write(dir.path().join("real_1.0.0.wasm"), wasm).unwrap();

        let mut reg = BlockRegistry::new();
        let results = reg.boot_discover(dir.path());

        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
        assert_eq!(reg.count(), 1);
    }
}
