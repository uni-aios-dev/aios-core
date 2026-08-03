//! High-level store operations combining sources and the on-disk installer.
use crate::catalog::{download_block, fetch_index};
use crate::installer::{cmp_version, BlockInstaller, InstalledBlock, UpdateInfo};
use crate::manifest::ManifestInfo;
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
            installer: BlockInstaller::new(blocks_dir),
        }
    }

    /// Create a manager with explicit sources.
    pub fn with_sources(sources: Vec<StoreSource>, blocks_dir: impl Into<PathBuf>) -> Self {
        Self {
            sources,
            installer: BlockInstaller::new(blocks_dir),
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

    fn client(&self) -> reqwest::Client {
        reqwest::Client::new()
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

    /// Run an async store operation from a synchronous context (e.g. the TUI shell).
    pub fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build store runtime");
        runtime.block_on(fut)
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
}
