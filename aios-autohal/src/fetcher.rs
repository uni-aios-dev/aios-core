use crate::adapter::{DriverLanguage, SourceAdapter};
use crate::catalog::{find_builtin, BuiltinDriver};
use crate::fingerprint::HardwareFingerprint;
use crate::manifest::{DriverManifest, DriverSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Errors raised while searching for and downloading a driver.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("driver not found for {0}")]
    NotFound(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("invalid driver manifest: {0}")]
    InvalidManifest(String),
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
}

/// A driver either arrives as a ready WASM module or as C/Rust source that the
/// adapter must rewrite and compile to `wasm32-wasi`.
#[derive(Debug, Clone)]
pub enum FetchedDriver {
    Wasm {
        manifest: DriverManifest,
        bytes: Vec<u8>,
    },
    Source {
        manifest: DriverManifest,
        language: DriverLanguage,
        code: String,
    },
}

/// One row of a remote driver catalog (`index.json`), the format shared by the
/// Redox Tree and Linux Core mirror endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverCatalogEntry {
    pub manifest: DriverManifest,
    #[serde(default)]
    pub wasm_path: Option<String>,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverCatalogIndex {
    pub drivers: Vec<DriverCatalogEntry>,
}

/// Backing transport. `Http` performs real requests through `reqwest`;
/// `Mock` serves a pre-populated URL -> bytes map and keeps the pipeline fully
/// deterministic in tests and offline builds.
#[derive(Clone)]
pub enum Transport {
    Http(reqwest::Client),
    Mock(Arc<Mutex<HashMap<String, Vec<u8>>>>),
}

impl Transport {
    pub async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        match self {
            Self::Http(client) => {
                let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("HTTP {}", resp.status().as_u16()));
                }
                resp.bytes()
                    .await
                    .map(|b| b.to_vec())
                    .map_err(|e| e.to_string())
            }
            Self::Mock(map) => {
                let map = map.lock().map_err(|e| e.to_string())?;
                map.get(url)
                    .cloned()
                    .ok_or_else(|| format!("mock transport: no route for '{url}'"))
            }
        }
    }

    /// Synchronous wrapper used by the block/IPC and UI paths; the pipeline is
    /// synchronous there and only the network hop needs a runtime.
    pub fn sync_get(&self, url: &str) -> Result<Vec<u8>, String> {
        let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        rt.block_on(self.get(url))
    }
}

impl Default for Transport {
    fn default() -> Self {
        Self::Http(reqwest::Client::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetcherConfig {
    /// Custom store registry root, e.g. `https://store.aios.dev`. Drivers live
    /// at `{root}/drivers/{driver_id}/driver.json` + `driver.wasm`.
    pub registry_url: Option<String>,
    /// Redox OS driver tree mirror exposing `index.json`.
    pub redox_tree_url: Option<String>,
    /// linux-hardware.org mirror exposing `index.json`.
    pub linux_core_url: Option<String>,
    /// Whether the offline builtin catalog is consulted before the network.
    pub use_builtin_catalog: bool,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            registry_url: None,
            redox_tree_url: None,
            linux_core_url: None,
            use_builtin_catalog: true,
        }
    }
}

/// On-demand driver fetcher: consults the offline builtin catalog, then the
/// configured remote registries, returning either WASM bytes or source code.
pub struct DriverFetcher {
    transport: Transport,
    adapter: SourceAdapter,
    config: FetcherConfig,
}

impl DriverFetcher {
    pub fn new(config: FetcherConfig) -> Self {
        Self {
            transport: Transport::default(),
            adapter: SourceAdapter::default(),
            config,
        }
    }

    pub fn with_transport(config: FetcherConfig, transport: Transport) -> Self {
        Self {
            transport,
            adapter: SourceAdapter::default(),
            config,
        }
    }

    pub fn config(&self) -> &FetcherConfig {
        &self.config
    }

    /// The source adapter used to transpile downloaded C/Rust drivers; the
    /// engine drives `adapt` + `compile` through this accessor.
    pub fn adapter(&self) -> &SourceAdapter {
        &self.adapter
    }

    /// Locate a driver for a fingerprint. Order: builtin catalog, custom
    /// store registry, redox tree, linux-core mirror.
    pub async fn find_driver(&self, fp: &HardwareFingerprint) -> Result<FetchedDriver, FetchError> {
        if self.config.use_builtin_catalog {
            if let Some(builtin) = find_builtin(fp) {
                return Ok(builtin_to_fetched(builtin));
            }
        }

        if let Some(registry) = self.config.registry_url.as_deref() {
            if let Some(driver) = self.fetch_from_registry(registry, fp).await? {
                return Ok(driver);
            }
        }

        if let Some(redox) = self.config.redox_tree_url.as_deref() {
            if let Some(driver) = self
                .fetch_from_catalog(redox, DriverSource::RedoxTree, fp)
                .await?
            {
                return Ok(driver);
            }
        }

        if let Some(linux) = self.config.linux_core_url.as_deref() {
            if let Some(driver) = self
                .fetch_from_catalog(linux, DriverSource::LinuxCore, fp)
                .await?
            {
                return Ok(driver);
            }
        }

        Err(FetchError::NotFound(fp.display_name()))
    }

    /// Synchronous variant for block/IPC and UI paths.
    pub fn find_driver_sync(&self, fp: &HardwareFingerprint) -> Result<FetchedDriver, FetchError> {
        let rt = tokio::runtime::Runtime::new().map_err(|e| FetchError::Network(e.to_string()))?;
        rt.block_on(self.find_driver(fp))
    }

    /// Custom store registry layout: `{root}/drivers/{id}/driver.json` +
    /// `driver.wasm`.
    async fn fetch_from_registry(
        &self,
        registry: &str,
        fp: &HardwareFingerprint,
    ) -> Result<Option<FetchedDriver>, FetchError> {
        let base = format!(
            "{}/drivers/{}",
            registry.trim_end_matches('/'),
            fp.driver_id()
        );
        let manifest_json = match self.transport.get(&format!("{base}/driver.json")).await {
            Ok(bytes) => bytes,
            Err(_) => return Ok(None),
        };
        let manifest = DriverManifest::from_json(&manifest_json)
            .map_err(|e| FetchError::InvalidManifest(e.to_string()))?;
        if !manifest.can_serve(fp) {
            return Ok(None);
        }
        let bytes = self
            .transport
            .get(&format!("{base}/driver.wasm"))
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if !manifest.hash_sha256.is_empty() && actual != manifest.hash_sha256 {
            return Err(FetchError::HashMismatch {
                expected: manifest.hash_sha256,
                actual,
            });
        }
        Ok(Some(FetchedDriver::Wasm { manifest, bytes }))
    }

    /// Generic `index.json` catalog mirror (Redox Tree / Linux Core).
    async fn fetch_from_catalog(
        &self,
        root: &str,
        source: DriverSource,
        fp: &HardwareFingerprint,
    ) -> Result<Option<FetchedDriver>, FetchError> {
        let root = root.trim_end_matches('/');
        let index_json = match self.transport.get(&format!("{root}/index.json")).await {
            Ok(bytes) => bytes,
            Err(_) => return Ok(None),
        };
        let index: DriverCatalogIndex = serde_json::from_slice(&index_json)
            .map_err(|e| FetchError::InvalidManifest(e.to_string()))?;
        let entry = match index.drivers.iter().find(|d| d.manifest.can_serve(fp)) {
            Some(entry) => entry,
            None => return Ok(None),
        };

        let mut manifest = entry.manifest.clone();
        manifest.source = source;
        manifest.validate().map_err(FetchError::InvalidManifest)?;

        if let Some(wasm_path) = entry.wasm_path.as_deref() {
            let bytes = self
                .transport
                .get(&format!("{root}/{wasm_path}"))
                .await
                .map_err(|e| FetchError::Network(e.to_string()))?;
            let actual = hex::encode(Sha256::digest(&bytes));
            if !manifest.hash_sha256.is_empty() && actual != manifest.hash_sha256 {
                return Err(FetchError::HashMismatch {
                    expected: manifest.hash_sha256,
                    actual,
                });
            }
            return Ok(Some(FetchedDriver::Wasm { manifest, bytes }));
        }

        if let Some(source_path) = entry.source_path.as_deref() {
            let code = self
                .transport
                .get(&format!("{root}/{source_path}"))
                .await
                .map_err(|e| FetchError::Network(e.to_string()))?;
            let language = match entry.language.as_deref().map(str::to_lowercase).as_deref() {
                Some("rust") | Some("rs") => DriverLanguage::Rust,
                _ => DriverLanguage::C,
            };
            let code = String::from_utf8_lossy(&code).to_string();
            return Ok(Some(FetchedDriver::Source {
                manifest,
                language,
                code,
            }));
        }

        Ok(None)
    }
}

fn builtin_to_fetched(builtin: BuiltinDriver) -> FetchedDriver {
    let bytes = builtin.wat_bytes();
    FetchedDriver::Wasm {
        manifest: builtin.manifest,
        bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::BusType;
    use crate::manifest::{DriverSource, SupportedHardware};
    use aios_security::capability::Capability;

    fn fp() -> HardwareFingerprint {
        HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x046D,
            device_id: 0x0825,
            class_code: 0,
            serial_or_acpi: None,
        }
    }

    fn sample_manifest() -> DriverManifest {
        DriverManifest {
            id: "driver.usb.046d.0825".into(),
            name: "Remote C270 Driver".into(),
            version: "2.0.0".into(),
            description: "fetched".into(),
            supported_hardware: vec![SupportedHardware {
                bus: "usb".into(),
                vendor_id: Some(0x046D),
                device_id: Some(0x0825),
            }],
            required_capabilities: vec![Capability::HwAccess],
            hash_sha256: hex::encode(Sha256::digest(b"wasm-binary")),
            entry_point: "_start_driver".into(),
            source: DriverSource::CustomStore,
            size_bytes: 11,
        }
    }

    fn mock_transport(urls: Vec<(&str, Vec<u8>)>) -> Transport {
        let map = urls.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        Transport::Mock(Arc::new(Mutex::new(map)))
    }

    #[test]
    fn test_builtin_catalog_first() {
        let fetcher = DriverFetcher::new(FetcherConfig::default());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let got = rt.block_on(fetcher.find_driver(&fp())).unwrap();
        match got {
            FetchedDriver::Wasm { manifest, bytes } => {
                assert_eq!(manifest.id, "driver.usb.046d.0825");
                assert_eq!(manifest.source, DriverSource::Builtin);
                assert!(!bytes.is_empty());
            }
            _ => panic!("expected wasm"),
        }
    }

    #[test]
    fn test_fetch_from_registry_mock() {
        let mut manifest = sample_manifest();
        manifest.hash_sha256 = hex::encode(Sha256::digest(b"wasm-binary"));
        let json = manifest.to_json().unwrap();
        let config = FetcherConfig {
            use_builtin_catalog: false,
            registry_url: Some("https://store.test".into()),
            ..Default::default()
        };
        let transport = mock_transport(vec![
            (
                "https://store.test/drivers/driver.usb.046d.0825/driver.json",
                json.into_bytes(),
            ),
            (
                "https://store.test/drivers/driver.usb.046d.0825/driver.wasm",
                b"wasm-binary".to_vec(),
            ),
        ]);
        let fetcher = DriverFetcher::with_transport(config, transport);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let got = rt.block_on(fetcher.find_driver(&fp())).unwrap();
        match got {
            FetchedDriver::Wasm { manifest, bytes } => {
                assert_eq!(manifest.name, "Remote C270 Driver");
                assert_eq!(bytes, b"wasm-binary");
            }
            _ => panic!("expected wasm"),
        }
    }

    #[test]
    fn test_registry_hash_mismatch_detected() {
        let mut manifest = sample_manifest();
        manifest.hash_sha256 = hex::encode(Sha256::digest(b"expected"));
        let json = manifest.to_json().unwrap();
        let config = FetcherConfig {
            use_builtin_catalog: false,
            registry_url: Some("https://store.test".into()),
            ..Default::default()
        };
        let transport = mock_transport(vec![
            (
                "https://store.test/drivers/driver.usb.046d.0825/driver.json",
                json.into_bytes(),
            ),
            (
                "https://store.test/drivers/driver.usb.046d.0825/driver.wasm",
                b"tampered-bytes".to_vec(),
            ),
        ]);
        let fetcher = DriverFetcher::with_transport(config, transport);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(fetcher.find_driver(&fp())).unwrap_err();
        assert!(matches!(err, FetchError::HashMismatch { .. }));
    }

    #[test]
    fn test_fetch_source_from_catalog() {
        let manifest = DriverManifest {
            hash_sha256: String::new(),
            ..sample_manifest()
        };
        let index = DriverCatalogIndex {
            drivers: vec![DriverCatalogEntry {
                manifest,
                wasm_path: None,
                source_path: Some("drivers/c270.c".into()),
                language: Some("c".into()),
            }],
        };
        let index_json = serde_json::to_vec(&index).unwrap();
        let config = FetcherConfig {
            use_builtin_catalog: false,
            redox_tree_url: Some("https://redox.test".into()),
            ..Default::default()
        };
        let transport = mock_transport(vec![
            ("https://redox.test/index.json", index_json),
            (
                "https://redox.test/drivers/c270.c",
                b"#include <stdio.h>".to_vec(),
            ),
        ]);
        let fetcher = DriverFetcher::with_transport(config, transport);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let got = rt.block_on(fetcher.find_driver(&fp())).unwrap();
        match got {
            FetchedDriver::Source {
                manifest,
                language,
                code,
            } => {
                assert_eq!(language, DriverLanguage::C);
                assert_eq!(manifest.source, DriverSource::RedoxTree);
                assert!(code.contains("stdio.h"));
            }
            _ => panic!("expected source"),
        }
    }

    #[test]
    fn test_not_found() {
        let config = FetcherConfig {
            use_builtin_catalog: false,
            ..Default::default()
        };
        let fetcher = DriverFetcher::with_transport(config, mock_transport(Vec::new()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(fetcher.find_driver(&fp())).unwrap_err();
        assert!(matches!(err, FetchError::NotFound(_)));
    }

    #[test]
    fn test_sync_get_mock() {
        let transport = mock_transport(vec![("https://x/1", b"data".to_vec())]);
        assert_eq!(transport.sync_get("https://x/1").unwrap(), b"data");
        assert!(transport.sync_get("https://x/2").is_err());
    }
}
