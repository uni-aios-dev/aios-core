//! High-level store operations combining sources and the on-disk installer.
use crate::catalog::{download_block, fetch_index};
use crate::installer::{cmp_version, BlockInstaller, InstalledBlock, UpdateInfo};
use crate::manifest::{ManifestInfo, ManifestValidator};
use crate::source::StoreSource;
use std::path::PathBuf;

/// Facade for searching, installing and updating blocks across multiple sources.
pub struct StoreManager {
    /// Configured sources; the first one is the default.
    pub sources: Vec<StoreSource>,
    /// On-disk installer.
    pub installer: BlockInstaller,
}

impl StoreManager {
    /// Create a manager with the default GitHub community source.
    pub fn new(blocks_dir: impl Into<PathBuf>) -> Self {
        Self {
            sources: vec![StoreSource::github_default()],
            installer: BlockInstaller::from_env(blocks_dir),
        }
    }

    /// Create a manager with explicit sources.
    pub fn with_sources(sources: Vec<StoreSource>, blocks_dir: impl Into<PathBuf>) -> Self {
        Self {
            sources,
            installer: BlockInstaller::from_env(blocks_dir),
        }
    }

    /// Create a manager bound to the default blocks directory.
    pub fn with_default_dir() -> Self {
        Self::new(BlockInstaller::default_dir())
    }

    /// Add a source, ignoring duplicates by name.
    pub fn add_source(&mut self, source: StoreSource) -> Result<(), String> {
        if self.sources.iter().any(|s| s.name == source.name) {
            return Err(format!("Source '{}' already registered", source.name));
        }
        self.sources.push(source);
        Ok(())
    }

    /// All configured sources.
    pub fn sources(&self) -> &[StoreSource] {
        &self.sources
    }

    /// Resolve a source by name; `None` selects the default (first) source.
    pub fn source(&self, name: Option<&str>) -> Result<&StoreSource, String> {
        match name {
            None => self
                .sources
                .first()
                .ok_or_else(|| "No sources configured".to_string()),
            Some(n) => self
                .sources
                .iter()
                .find(|s| s.name == n)
                .ok_or_else(|| format!("Source '{n}' not found")),
        }
    }

    /// Add trusted public keys to a source. Keys already present are ignored.
    ///
    /// After a source has trusted keys, every manifest installed from it must
    /// be signed by one of them (see [`StoreManager::verify_source_manifest`]).
    pub fn trust_source(&mut self, source_name: &str, keys: &[String]) -> Result<(), String> {
        let source = self
            .sources
            .iter_mut()
            .find(|s| s.name == source_name)
            .ok_or_else(|| format!("Source '{source_name}' not found"))?;
        for key in keys {
            if !source.trusted_public_keys.iter().any(|k| k == key) {
                source.trusted_public_keys.push(key.clone());
            }
        }
        Ok(())
    }

    /// Remove all trusted keys from a source, returning how many were removed.
    /// An empty key list re-allows unsigned manifests from that source.
    pub fn clear_source_trust(&mut self, source_name: &str) -> Result<usize, String> {
        let source = self
            .sources
            .iter_mut()
            .find(|s| s.name == source_name)
            .ok_or_else(|| format!("Source '{source_name}' not found"))?;
        let removed = source.trusted_public_keys.len();
        source.trusted_public_keys.clear();
        Ok(removed)
    }

    /// Persist the configured sources (including trusted keys) as JSON so a
    /// fresh [`StoreManager`] can be rebuilt with the same trust policy.
    pub fn save_config(&self, path: &std::path::Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.sources)
            .map_err(|e| format!("Serialize failed: {e}"))?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        std::fs::write(path, json).map_err(|e| format!("Write failed: {e}"))
    }

    /// Rebuild a manager from a config file written by
    /// [`StoreManager::save_config`]. Falls back to the default source set on
    /// any error (missing file, corrupt JSON).
    pub fn load_config(
        path: &std::path::Path,
        blocks_dir: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let sources: Vec<StoreSource> =
            serde_json::from_str(&data).map_err(|e| format!("Parse failed: {e}"))?;
        Ok(Self::with_sources(sources, blocks_dir))
    }

    fn client(&self) -> reqwest::Client {
        reqwest::Client::new()
    }

    /// Verify a downloaded manifest against the source's trust policy before
    /// installation.
    ///
    /// - Source has trusted keys → the manifest must be signed by one of them.
    /// - Source has no trusted keys → an embedded signature is still verified
    ///   if present (defense in depth); unsigned manifests are allowed.
    fn verify_source_manifest(source: &StoreSource, manifest: &ManifestInfo) -> Result<(), String> {
        if source.trusted_public_keys.is_empty() {
            if manifest.signature.is_some() {
                let ok = ManifestValidator::verify_signature(manifest)
                    .map_err(|e| format!("Signature verification failed: {e}"))?;
                if !ok {
                    return Err(format!(
                        "Manifest '{}' has an invalid signature",
                        manifest.name
                    ));
                }
            }
            return Ok(());
        }
        let ok =
            ManifestValidator::verify_signature_with_keys(manifest, &source.trusted_public_keys)
                .map_err(|e| format!("Signature verification failed: {e}"))?;
        if !ok {
            return Err(format!(
                "Manifest '{}' is not signed by a key trusted by source '{}'",
                manifest.name,
                source.display()
            ));
        }
        Ok(())
    }

    /// Fetch the catalog of a source (default first source when `None`).
    pub async fn catalog(&self, source_name: Option<&str>) -> Result<Vec<ManifestInfo>, String> {
        let source = self.source(source_name)?;
        fetch_index(source, &self.client()).await
    }

    /// Search block names, descriptions and authors for `query`.
    pub async fn search(
        &self,
        query: &str,
        source_name: Option<&str>,
    ) -> Result<Vec<ManifestInfo>, String> {
        let catalog = self.catalog(source_name).await?;
        let q = query.to_lowercase();
        Ok(catalog
            .into_iter()
            .filter(|m| {
                m.name.to_lowercase().contains(&q)
                    || m.description.to_lowercase().contains(&q)
                    || m.author.to_lowercase().contains(&q)
            })
            .collect())
    }

    /// Download and install a block; newest version when `version` is `None`.
    pub async fn install(
        &mut self,
        source_name: Option<&str>,
        block_name: &str,
        version: Option<&str>,
    ) -> Result<InstalledBlock, String> {
        let catalog = self.catalog(source_name).await?;
        let candidates: Vec<ManifestInfo> = catalog
            .into_iter()
            .filter(|m| m.name == block_name)
            .collect();
        if candidates.is_empty() {
            return Err(format!("Block '{block_name}' not found in the catalog"));
        }
        let manifest = match version {
            Some(v) => candidates
                .into_iter()
                .find(|m| m.version == v)
                .ok_or_else(|| format!("Version '{v}' of '{block_name}' not found"))?,
            None => candidates
                .into_iter()
                .max_by(|a, b| cmp_version(&a.version, &b.version))
                .expect("candidates is non-empty"),
        };
        let source = self.source(source_name)?.clone();
        let binary = download_block(&source, &manifest.name, &self.client()).await?;
        Self::verify_source_manifest(&source, &manifest)?;
        if let Some(already) = self.installer.find_installed(&manifest.name) {
            if cmp_version(&already.manifest.version, &manifest.version) != std::cmp::Ordering::Less
            {
                log::info!(
                    "StoreManager: '{}' already at version {} (requested {})",
                    manifest.name,
                    already.manifest.version,
                    manifest.version
                );
                return Ok(already);
            }
        }
        self.installer.install_from_bytes(manifest, &binary)
    }

    /// Update one or all installed blocks from a source, rolling back on failure.
    pub async fn update(
        &mut self,
        source_name: Option<&str>,
        block_name: Option<&str>,
    ) -> Result<Vec<InstalledBlock>, String> {
        let catalog = self.catalog(source_name).await?;
        let updates = self.installer.check_updates(&catalog);
        let targets: Vec<&UpdateInfo> = match block_name {
            Some(name) => updates
                .iter()
                .filter(|u| u.installed.name == name)
                .collect(),
            None => updates.iter().collect(),
        };
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let source = self.source(source_name)?.clone();
        let client = self.client();
        let mut updated = Vec::new();
        for update in targets {
            let name = update.available.name.clone();
            let binary = match download_block(&source, &name, &client).await {
                Ok(b) => b,
                Err(e) => return Err(format!("Download of '{name}' failed: {e}")),
            };
            Self::verify_source_manifest(&source, &update.available)?;
            let _ = self.installer.backup(&name);
            match self
                .installer
                .install_from_bytes(update.available.clone(), &binary)
            {
                Ok(block) => updated.push(block),
                Err(e) => {
                    let _ = self.installer.rollback(&name);
                    return Err(format!(
                        "Update of '{name}' failed and was rolled back: {e}"
                    ));
                }
            }
        }
        Ok(updated)
    }

    /// Check which installed blocks have newer versions available.
    pub async fn check_updates(
        &self,
        source_name: Option<&str>,
    ) -> Result<Vec<UpdateInfo>, String> {
        let catalog = self.catalog(source_name).await?;
        Ok(self.installer.check_updates(&catalog))
    }

    /// Installed blocks (newest version per name).
    pub fn list_installed(&self) -> Vec<InstalledBlock> {
        self.installer.list_installed()
    }

    /// Highest-versioned installation of `name`, if any.
    pub fn find_installed(&self, name: &str) -> Option<InstalledBlock> {
        self.installer.find_installed(name)
    }

    /// Uninstall every version of `name`.
    pub fn uninstall(&mut self, name: &str) -> Result<Vec<InstalledBlock>, String> {
        self.installer.uninstall(name)
    }

    /// Restore the previous version from a `.bak` backup.
    pub fn rollback(&mut self, name: &str) -> Result<InstalledBlock, String> {
        self.installer.rollback(name)
    }

    /// Parse a source spec string like `github:owner/repo`, `local:path` or
    /// `http://host:port` into a [`StoreSource`].
    pub fn parse_source_spec(spec: &str) -> Result<StoreSource, String> {
        let s = spec.trim();
        if let Some(rest) = s.strip_prefix("github:") {
            return Ok(StoreSource::github(rest));
        }
        if let Some(rest) = s.strip_prefix("local:") {
            return Ok(StoreSource::local(rest));
        }
        if s.starts_with("http://") || s.starts_with("https://") {
            return Ok(StoreSource::http(s));
        }
        Err(format!(
            "Unrecognized source spec '{spec}' (use github:owner/repo, local:path or http://url)"
        ))
    }

    /// Run an async store operation from a synchronous context (e.g. the TUI
    /// shell). Safe to call from inside a tokio runtime (see
    /// [`aios_core::runtime::block_on_future`]).
    pub fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        aios_core::runtime::block_on_future(fut)
    }
}

impl Default for StoreManager {
    fn default() -> Self {
        Self::with_default_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceKind;
    use sha2::{Digest, Sha256};
    use std::path::Path;

    fn write_source_block(dir: &Path, name: &str, version: &str, binary: &[u8]) {
        let block_dir = dir.join("blocks");
        std::fs::create_dir_all(&block_dir).unwrap();
        std::fs::write(block_dir.join(format!("{name}_{version}.wasm")), binary).unwrap();
    }

    fn source_manifest(name: &str, version: &str, binary: &[u8]) -> ManifestInfo {
        ManifestInfo {
            name: name.into(),
            version: version.into(),
            description: "store block".into(),
            author: "store".into(),
            capabilities: std::collections::HashSet::new(),
            wasm_size_bytes: binary.len() as u64,
            wasm_sha256: hex::encode(Sha256::digest(binary)),
            signature: None,
            store_url: None,
        }
    }

    #[test]
    fn test_parse_source_spec() {
        let github = StoreManager::parse_source_spec("github:acme/store").unwrap();
        assert_eq!(github.kind, SourceKind::GitHub);
        let local = StoreManager::parse_source_spec("local:C:/blocks").unwrap();
        assert_eq!(local.kind, SourceKind::Local);
        let http = StoreManager::parse_source_spec("https://store.example").unwrap();
        assert_eq!(http.kind, SourceKind::Http);
        assert!(StoreManager::parse_source_spec("bogus").is_err());
    }

    #[test]
    fn test_add_source_dedupes() {
        let mut manager = StoreManager::new(tempfile::tempdir().unwrap().path());
        manager
            .add_source(StoreSource::github_default())
            .unwrap_err();
        manager
            .add_source(StoreSource::github("acme/other"))
            .unwrap();
        assert_eq!(manager.sources().len(), 2);
    }

    #[test]
    fn test_source_resolution() {
        let mut manager = StoreManager::new(tempfile::tempdir().unwrap().path());
        manager.sources = vec![StoreSource::local("C:/a"), StoreSource::local("C:/b")];
        assert_eq!(manager.source(None).unwrap().base, "C:/a");
        assert_eq!(manager.source(Some("local:C:/b")).unwrap().base, "C:/b");
        assert!(manager.source(Some("nope")).is_err());
    }

    #[test]
    fn test_trust_and_clear_source_keys() {
        use ed25519_dalek::SigningKey;
        use rand_core::OsRng;

        let key = hex::encode(SigningKey::generate(&mut OsRng).verifying_key().to_bytes());
        let mut manager = StoreManager::new(tempfile::tempdir().unwrap().path());
        let name = manager.sources[0].name.clone();

        manager
            .trust_source(&name, std::slice::from_ref(&key))
            .unwrap();
        assert_eq!(
            manager.source(Some(&name)).unwrap().trusted_public_keys,
            vec![key.clone()]
        );

        manager
            .trust_source(&name, std::slice::from_ref(&key))
            .unwrap();
        assert_eq!(
            manager
                .source(Some(&name))
                .unwrap()
                .trusted_public_keys
                .len(),
            1
        );

        manager.trust_source(&name, &["deadbeef".into()]).unwrap();
        assert_eq!(
            manager
                .source(Some(&name))
                .unwrap()
                .trusted_public_keys
                .len(),
            2
        );

        assert!(manager.trust_source("nope", &[key]).is_err());
        assert_eq!(manager.clear_source_trust(&name).unwrap(), 2);
        assert!(manager
            .source(Some(&name))
            .unwrap()
            .trusted_public_keys
            .is_empty());
        assert!(manager.clear_source_trust("nope").is_err());
    }

    #[test]
    fn test_save_and_load_config_roundtrip() {
        use ed25519_dalek::SigningKey;
        use rand_core::OsRng;

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("store_config.json");
        let key = hex::encode(SigningKey::generate(&mut OsRng).verifying_key().to_bytes());

        let mut manager = StoreManager::new(dir.path());
        let name = manager.sources[0].name.clone();
        manager
            .trust_source(&name, std::slice::from_ref(&key))
            .unwrap();
        manager.save_config(&cfg).unwrap();

        let reloaded = StoreManager::load_config(&cfg, dir.path()).unwrap();
        assert_eq!(reloaded.sources.len(), 1);
        assert_eq!(reloaded.sources[0].name, name);
        assert_eq!(reloaded.sources[0].trusted_public_keys, vec![key]);
        assert!(reloaded.installer.blocks_dir.exists());

        assert!(StoreManager::load_config(&dir.path().join("missing.json"), dir.path()).is_err());
    }

    #[test]
    fn test_install_from_local_source() {
        let src_dir = tempfile::tempdir().unwrap();
        let blocks_dir = tempfile::tempdir().unwrap();
        write_source_block(src_dir.path(), "net", "1.2.0", b"wasm-net");
        std::fs::write(
            src_dir.path().join("index.json"),
            serde_json::to_vec(&vec![source_manifest("net", "1.2.0", b"wasm-net")]).unwrap(),
        )
        .unwrap();

        let mut manager = StoreManager::with_sources(
            vec![StoreSource::local(src_dir.path().to_str().unwrap())],
            blocks_dir.path(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let installed = runtime
            .block_on(manager.install(None, "net", None))
            .unwrap();
        assert_eq!(installed.manifest.version, "1.2.0");
        assert_eq!(std::fs::read(&installed.path).unwrap(), b"wasm-net");
        assert!(manager.find_installed("net").is_some());
    }

    #[test]
    fn test_search_filters() {
        let src_dir = tempfile::tempdir().unwrap();
        let catalog = vec![
            source_manifest("browser", "1.0.0", b"b"),
            source_manifest("search-tool", "1.0.0", b"s"),
        ];
        std::fs::write(
            src_dir.path().join("index.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        let manager = StoreManager::with_sources(
            vec![StoreSource::local(src_dir.path().to_str().unwrap())],
            tempfile::tempdir().unwrap().path(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let results = runtime.block_on(manager.search("brow", None)).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "browser");
    }

    #[test]
    fn test_install_missing_block_errors() {
        let src_dir = tempfile::tempdir().unwrap();
        std::fs::write(src_dir.path().join("index.json"), b"[]").unwrap();
        let mut manager = StoreManager::with_sources(
            vec![StoreSource::local(src_dir.path().to_str().unwrap())],
            tempfile::tempdir().unwrap().path(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(runtime
            .block_on(manager.install(None, "missing", None))
            .is_err());
    }

    #[test]
    fn test_update_and_rollback_via_manager() {
        let src_dir = tempfile::tempdir().unwrap();
        let blocks_dir = tempfile::tempdir().unwrap();
        write_source_block(src_dir.path(), "app", "1.0.0", b"old");
        std::fs::write(
            src_dir.path().join("index.json"),
            serde_json::to_vec(&vec![source_manifest("app", "1.0.0", b"old")]).unwrap(),
        )
        .unwrap();

        let mut manager = StoreManager::with_sources(
            vec![StoreSource::local(src_dir.path().to_str().unwrap())],
            blocks_dir.path(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(manager.install(None, "app", None))
            .unwrap();

        write_source_block(src_dir.path(), "app", "2.0.0", b"new");
        std::fs::write(
            src_dir.path().join("index.json"),
            serde_json::to_vec(&vec![source_manifest("app", "2.0.0", b"new")]).unwrap(),
        )
        .unwrap();

        let updates = runtime.block_on(manager.check_updates(None)).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].available.version, "2.0.0");

        let updated = runtime.block_on(manager.update(None, Some("app"))).unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].manifest.version, "2.0.0");
        assert_eq!(std::fs::read(&updated[0].path).unwrap(), b"new");

        let restored = manager.rollback("app").unwrap();
        assert_eq!(restored.manifest.version, "1.0.0");
        assert_eq!(std::fs::read(&restored.path).unwrap(), b"old");
    }

    #[test]
    fn test_install_rejects_untrusted_signature_from_source() {
        use crate::manifest::sign_manifest;
        use ed25519_dalek::SigningKey;
        use rand_core::OsRng;

        let src_dir = tempfile::tempdir().unwrap();
        let blocks_dir = tempfile::tempdir().unwrap();
        let signer = SigningKey::generate(&mut OsRng);
        let trusted = SigningKey::generate(&mut OsRng);

        write_source_block(src_dir.path(), "net", "1.0.0", b"wasm-net");
        let mut manifest = source_manifest("net", "1.0.0", b"wasm-net");
        manifest.signature = Some(sign_manifest(&manifest, &signer));
        std::fs::write(
            src_dir.path().join("index.json"),
            serde_json::to_vec(&vec![manifest]).unwrap(),
        )
        .unwrap();

        let mut source = StoreSource::local(src_dir.path().to_str().unwrap());
        source.trusted_public_keys = vec![hex::encode(trusted.verifying_key().to_bytes())];
        let mut manager = StoreManager::with_sources(vec![source], blocks_dir.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime
            .block_on(manager.install(None, "net", None))
            .unwrap_err();
        assert!(err.contains("not signed by a key trusted"), "got: {err}");
    }

    #[test]
    fn test_install_accepts_trusted_signature_from_source() {
        use crate::manifest::sign_manifest;
        use ed25519_dalek::SigningKey;
        use rand_core::OsRng;

        let src_dir = tempfile::tempdir().unwrap();
        let blocks_dir = tempfile::tempdir().unwrap();
        let signer = SigningKey::generate(&mut OsRng);

        write_source_block(src_dir.path(), "net", "1.0.0", b"wasm-net");
        let mut manifest = source_manifest("net", "1.0.0", b"wasm-net");
        manifest.signature = Some(sign_manifest(&manifest, &signer));
        std::fs::write(
            src_dir.path().join("index.json"),
            serde_json::to_vec(&vec![manifest]).unwrap(),
        )
        .unwrap();

        let mut source = StoreSource::local(src_dir.path().to_str().unwrap());
        source.trusted_public_keys = vec![hex::encode(signer.verifying_key().to_bytes())];
        let mut manager = StoreManager::with_sources(vec![source], blocks_dir.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let installed = runtime
            .block_on(manager.install(None, "net", None))
            .unwrap();
        assert_eq!(installed.manifest.version, "1.0.0");
        assert_eq!(std::fs::read(&installed.path).unwrap(), b"wasm-net");
    }
}
