use crate::registry::{BlockEntry, BlockRegistry};
use aios_core::block::{BlockId, BlockManifest, BlockState};
use aios_core::crypto;
use aios_core::error::{AIOSException, Result};
use aios_security::capability::{Capability, CapabilityToken};
use std::path::Path;

#[derive(serde::Deserialize)]
pub struct BlockManifestJson {
    pub name: Option<String>,
    pub version: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub ttl_ms: Option<u64>,
}

impl BlockManifestJson {
    pub fn from_file(path: &Path) -> Option<Self> {
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn parse_capabilities(&self) -> Option<Vec<Capability>> {
        let caps = self.capabilities.as_ref()?;
        let parsed: Vec<Capability> = caps
            .iter()
            .filter_map(|s| match s.as_str() {
                "CAP_NET_BIND" => Some(Capability::NetBind),
                "CAP_NET_CONNECT" => Some(Capability::NetConnect),
                "CAP_NET_LISTEN" => Some(Capability::NetListen),
                "CAP_FS_READ" => Some(Capability::FsRead),
                "CAP_FS_WRITE" => Some(Capability::FsWrite),
                "CAP_FS_DELETE" => Some(Capability::FsDelete),
                "CAP_HW_ACCESS" => Some(Capability::HwAccess),
                "CAP_MEM_ALLOC" => Some(Capability::MemAlloc),
                "CAP_MEM_SHARE" => Some(Capability::MemShare),
                "CAP_SCHED_MODIFY" => Some(Capability::SchedModify),
                "CAP_BLOCK_LOAD" => Some(Capability::BlockLoad),
                "CAP_BLOCK_UNLOAD" => Some(Capability::BlockUnload),
                "CAP_PROCESS_SPAWN" => Some(Capability::ProcessSpawn),
                "CAP_PROCESS_KILL" => Some(Capability::ProcessKill),
                "CAP_SYSTEM_CONFIG" => Some(Capability::SystemConfig),
                "CAP_ALL" => Some(Capability::All),
                _ => None,
            })
            .collect();
        if parsed.is_empty() {
            None
        } else {
            Some(parsed)
        }
    }
}

pub struct BlockLoader;

impl BlockLoader {
    pub fn validate_binary(binary: &[u8], expected_sha256: &[u8; 32]) -> Result<()> {
        let actual = crypto::compute_sha256_bytes(binary);
        if actual == *expected_sha256 {
            log::debug!("BlockLoader: Binary validated, hash matches");
            Ok(())
        } else {
            Err(AIOSException::InvalidSignature {
                expected: hex::encode(expected_sha256),
                actual: hex::encode(actual),
            })
        }
    }

    pub fn load_from_binary(
        registry: &mut BlockRegistry,
        name: &str,
        version: &str,
        binary: Vec<u8>,
    ) -> Result<BlockManifest> {
        let id = registry.register_block(name, version, binary.clone())?;
        let entry = registry.get(id)?;
        Self::validate_binary(&binary, &entry.manifest.sha256)?;
        registry.activate_block(id)?;
        let entry = registry.get(id)?;
        log::info!(
            "BlockLoader: Loaded and activated block '{}' ({})",
            name,
            id
        );
        Ok(entry.manifest.clone())
    }

    pub fn load_from_directory(
        registry: &mut BlockRegistry,
        dir: &Path,
    ) -> Vec<Result<BlockManifest>> {
        let mut results = Vec::new();

        if !dir.exists() {
            log::warn!("BlockLoader: blocks directory {:?} does not exist", dir);
            return results;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                log::error!("BlockLoader: failed to read directory {:?}: {}", dir, e);
                return results;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
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
            let mut version = if parts.len() > 1 {
                parts[1].to_string()
            } else {
                "0.0.0".to_string()
            };
            let mut name = parts[0].to_string();

            let manifest_path = path.with_extension("json");
            let manifest_json = BlockManifestJson::from_file(&manifest_path);
            let mut assigned_token: Option<CapabilityToken> = None;

            if let Some(ref mj) = manifest_json {
                if let Some(ref n) = mj.name {
                    name = n.clone();
                }
                if let Some(ref v) = mj.version {
                    version = v.clone();
                }
                if let Some(caps) = mj.parse_capabilities() {
                    let ttl = mj.ttl_ms.unwrap_or(3_600_000);
                    assigned_token = Some(CapabilityToken::new(
                        0,
                        caps,
                        ttl,
                        b"aios_manifest_signing_key",
                    ));
                    log::info!(
                        "BlockLoader: manifest for '{}' assigns {} capabilities",
                        name,
                        assigned_token.as_ref().unwrap().capabilities.len()
                    );
                }
            }

            match std::fs::read(&path) {
                Ok(binary) => {
                    log::info!(
                        "BlockLoader: loading block '{}' v{} from {:?} ({} bytes)",
                        name,
                        version,
                        path,
                        binary.len()
                    );
                    let result = Self::load_from_binary_with_capabilities(
                        registry,
                        &name,
                        &version,
                        binary,
                        assigned_token,
                    );
                    results.push(result);
                }
                Err(e) => {
                    log::error!("BlockLoader: failed to read {:?}: {}", path, e);
                    results.push(Err(AIOSException::IPCError(format!(
                        "Failed to read block file {:?}: {}",
                        path, e
                    ))));
                }
            }
        }

        log::info!(
            "BlockLoader: loaded {} blocks from {:?}",
            results.len(),
            dir
        );
        results
    }

    pub fn load_from_binary_with_capabilities(
        registry: &mut BlockRegistry,
        name: &str,
        version: &str,
        binary: Vec<u8>,
        token: Option<CapabilityToken>,
    ) -> Result<BlockManifest> {
        let id = registry.register_block(name, version, binary.clone())?;
        if let Some(t) = token {
            registry.assign_capabilities(id, t)?;
        }
        let entry = registry.get(id)?;
        Self::validate_binary(&binary, &entry.manifest.sha256)?;
        registry.activate_block(id)?;
        let entry = registry.get(id)?;
        log::info!(
            "BlockLoader: Loaded and activated block '{}' ({})",
            name,
            id
        );
        Ok(entry.manifest.clone())
    }

    pub fn unload_block(registry: &mut BlockRegistry, id: BlockId) -> Result<BlockEntry> {
        let entry = registry.get(id)?;
        if entry.state == BlockState::Active {
            log::warn!(
                "BlockLoader: Unloading active block '{}', state should be extracted first",
                entry.manifest.name
            );
        }
        registry.unload_block(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_binary_ok() {
        let data = b"module binary data";
        let hash = crypto::compute_sha256_bytes(data);
        assert!(BlockLoader::validate_binary(data, &hash).is_ok());
    }

    #[test]
    fn test_validate_binary_fail() {
        let data = b"module binary data";
        let bad_hash = [0u8; 32];
        assert!(BlockLoader::validate_binary(data, &bad_hash).is_err());
    }

    #[test]
    fn test_load_from_binary() {
        let mut reg = BlockRegistry::new();
        let binary = b"test module".to_vec();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "test_module", "1.0.0", binary).unwrap();
        assert_eq!(manifest.name, "test_module");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(reg.get(manifest.id).unwrap().state, BlockState::Active);
    }

    #[test]
    fn test_unload_block() {
        let mut reg = BlockRegistry::new();
        let binary = b"test module".to_vec();
        let manifest =
            BlockLoader::load_from_binary(&mut reg, "test_module", "1.0.0", binary).unwrap();
        let entry = BlockLoader::unload_block(&mut reg, manifest.id).unwrap();
        assert_eq!(entry.manifest.name, "test_module");
        assert!(reg.get(manifest.id).is_err());
    }

    #[test]
    fn test_manifest_json_parse_capabilities() {
        let json = BlockManifestJson {
            name: Some("net_block".into()),
            version: Some("2.0.0".into()),
            capabilities: Some(vec!["CAP_NET_BIND".into(), "CAP_NET_CONNECT".into()]),
            ttl_ms: Some(120_000),
        };
        let caps = json.parse_capabilities().unwrap();
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(&aios_security::capability::Capability::NetBind));
        assert!(caps.contains(&aios_security::capability::Capability::NetConnect));
    }

    #[test]
    fn test_manifest_json_empty_caps() {
        let json = BlockManifestJson {
            name: None,
            version: None,
            capabilities: Some(vec![]),
            ttl_ms: None,
        };
        assert!(json.parse_capabilities().is_none());
    }

    #[test]
    fn test_manifest_json_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("block.json");
        std::fs::write(
            &manifest_path,
            r#"{"name":"custom_name","version":"3.0.0","capabilities":["CAP_FS_READ"],"ttl_ms":60000}"#,
        )
        .unwrap();

        let parsed = BlockManifestJson::from_file(&manifest_path).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("custom_name"));
        assert_eq!(parsed.version.as_deref(), Some("3.0.0"));
        assert_eq!(parsed.ttl_ms, Some(60_000));
        assert_eq!(parsed.capabilities.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_load_from_directory_with_sidecar_manifest() {
        let dir = tempfile::tempdir().unwrap();

        let wasm = r#"
            (module (func (export "init")))
        "#
        .as_bytes();
        std::fs::write(dir.path().join("mynet_1.0.0.wasm"), wasm).unwrap();
        std::fs::write(
            dir.path().join("mynet_1.0.0.json"),
            r#"{"name":"mynet_block","version":"1.5.0","capabilities":["CAP_NET_BIND","CAP_NET_CONNECT"]}"#,
        )
        .unwrap();

        let mut reg = BlockRegistry::new();
        let results = BlockLoader::load_from_directory(&mut reg, dir.path());

        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());

        let manifest = results[0].as_ref().unwrap();
        assert_eq!(manifest.name, "mynet_block");
        assert_eq!(manifest.version, "1.5.0");

        let entry = reg.get(manifest.id).unwrap();
        assert!(entry.capabilities.is_some());
        let token = entry.capabilities.as_ref().unwrap();
        assert!(token.has_capability(&aios_security::capability::Capability::NetBind));
        assert!(token.has_capability(&aios_security::capability::Capability::NetConnect));
        assert!(!token.has_capability(&aios_security::capability::Capability::FsRead));
    }

    #[test]
    fn test_load_from_directory_without_manifest_falls_back() {
        let dir = tempfile::tempdir().unwrap();

        let wasm = r#"
            (module (func (export "init")))
        "#
        .as_bytes();
        std::fs::write(dir.path().join("plain_1.0.0.wasm"), wasm).unwrap();

        let mut reg = BlockRegistry::new();
        let results = BlockLoader::load_from_directory(&mut reg, dir.path());

        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
        let manifest = results[0].as_ref().unwrap();
        assert_eq!(manifest.name, "plain");
        assert_eq!(manifest.version, "1.0.0");

        let entry = reg.get(manifest.id).unwrap();
        assert!(entry.capabilities.is_none());
    }
}
