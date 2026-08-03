//! On-disk block installation, update checks and rollback.
use crate::catalog::parse_name_version;
use crate::manifest::ManifestInfo;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A block installed in the local blocks directory.
#[derive(Debug, Clone)]
pub struct InstalledBlock {
    /// Block manifest (name, version, checksum, ...).
    pub manifest: ManifestInfo,
    /// Absolute path of the installed binary.
    pub path: PathBuf,
}

/// A pending update for an installed block.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// Currently installed version.
    pub installed: ManifestInfo,
    /// Newer version available in the catalog.
    pub available: ManifestInfo,
}

/// Compare two version strings using numeric dot components.
pub fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |v: &str| {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let pa = split(a);
    let pb = split(b);
    for (x, y) in pa.iter().zip(pb.iter()) {
        if x != y {
            return x.cmp(y);
        }
    }
    pa.len().cmp(&pb.len())
}

/// Manages installed block files in a local blocks directory.
///
/// Files follow the `<name>_<version>.wasm` naming convention understood by
/// `aios-block-mgr`; each binary has a `<name>_<version>.json` sidecar with the
/// full [`ManifestInfo`]. Older versions of the same block are kept on disk so
/// a failed update can be rolled back.
pub struct BlockInstaller {
    /// Directory holding installed block binaries and sidecars.
    pub blocks_dir: PathBuf,
}

impl BlockInstaller {
    /// Create an installer bound to `blocks_dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            blocks_dir: dir.into(),
        }
    }

    /// Default blocks directory: `AIOS_BLOCKS_DIR` or `./blocks`.
    pub fn default_dir() -> PathBuf {
        std::env::var_os("AIOS_BLOCKS_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("blocks"))
    }

    /// Ensure the blocks directory exists.
    pub fn ensure_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.blocks_dir)
            .map_err(|e| format!("Failed to create {:?}: {e}", self.blocks_dir))
    }

    /// Sanitize a block/version string into a safe file-name component.
    pub fn sanitize_component(input: &str) -> String {
        let mut out: String = input
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        while out.ends_with('.') {
            out.pop();
        }
        out
    }

    /// Install a binary together with its manifest, verifying the SHA-256.
    pub fn install_from_bytes(
        &mut self,
        manifest: ManifestInfo,
        binary: &[u8],
    ) -> Result<InstalledBlock, String> {
        self.ensure_dir()?;
        let name = Self::sanitize_component(&manifest.name);
        let version = Self::sanitize_component(&manifest.version);
        if name.is_empty() || version.is_empty() {
            return Err("Block name and version must not be empty".to_string());
        }
        let actual = hex::encode(Sha256::digest(binary));
        if !manifest.wasm_sha256.is_empty() && actual != manifest.wasm_sha256 {
            return Err(format!(
                "SHA-256 mismatch for '{}': expected {}, got {}",
                manifest.name, manifest.wasm_sha256, actual
            ));
        }

        let path = self.blocks_dir.join(format!("{name}_{version}.wasm"));
        std::fs::write(&path, binary).map_err(|e| format!("Failed to write {:?}: {e}", path))?;
        self.write_sidecar(&manifest)?;
        log::info!(
            "BlockInstaller: installed '{}' v{} ({}) from {} bytes",
            manifest.name,
            manifest.version,
            path.display(),
            binary.len()
        );
        Ok(InstalledBlock { manifest, path })
    }

    /// Install a block from a local file, computing the manifest automatically.
    pub fn install_from_file(
        &mut self,
        src: &Path,
        name: &str,
        version: &str,
    ) -> Result<InstalledBlock, String> {
        let binary = std::fs::read(src).map_err(|e| format!("Failed to read {:?}: {e}", src))?;
        let manifest = ManifestInfo {
            name: name.to_string(),
            version: version.to_string(),
            description: String::new(),
            author: "local".into(),
            capabilities: HashSet::new(),
            wasm_size_bytes: binary.len() as u64,
            wasm_sha256: hex::encode(Sha256::digest(&binary)),
            signature: None,
            store_url: None,
        };
        self.install_from_bytes(manifest, &binary)
    }

    /// Write the JSON sidecar manifest next to an installed binary.
    pub fn write_sidecar(&self, manifest: &ManifestInfo) -> Result<(), String> {
        let name = Self::sanitize_component(&manifest.name);
        let version = Self::sanitize_component(&manifest.version);
        let sidecar = self.blocks_dir.join(format!("{name}_{version}.json"));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let json = serde_json::json!({
            "name": manifest.name,
            "version": manifest.version,
            "description": manifest.description,
            "author": manifest.author,
            "capabilities": manifest.capabilities,
            "wasm_size_bytes": manifest.wasm_size_bytes,
            "wasm_sha256": manifest.wasm_sha256,
            "installed_at": now,
        });
        std::fs::write(
            &sidecar,
            serde_json::to_string_pretty(&json).unwrap_or_default(),
        )
        .map_err(|e| format!("Failed to write {:?}: {e}", sidecar))
    }

    fn sidecar_path(&self, path: &Path) -> PathBuf {
        path.with_extension("json")
    }

    /// List all block binaries on disk, newest version first per name.
    pub fn list_installed(&self) -> Vec<InstalledBlock> {
        let mut entries = Vec::new();
        if !self.blocks_dir.is_dir() {
            return entries;
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.blocks_dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();

        for path in files {
            let ext = path.extension().and_then(|e| e.to_str());
            if ext != Some("wasm") && ext != Some("bin") {
                continue;
            }
            let (name, version) = parse_name_version(&path);
            let mut manifest = ManifestInfo {
                name,
                version,
                description: String::new(),
                author: "local".into(),
                capabilities: HashSet::new(),
                wasm_size_bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                wasm_sha256: String::new(),
                signature: None,
                store_url: None,
            };
            let sidecar = self.sidecar_path(&path);
            if sidecar.is_file() {
                if let Ok(data) = std::fs::read_to_string(&sidecar) {
                    if let Ok(m) = serde_json::from_str::<ManifestInfo>(&data) {
                        manifest = m;
                    }
                }
            }
            if manifest.wasm_sha256.is_empty() {
                if let Ok(binary) = std::fs::read(&path) {
                    manifest.wasm_sha256 = hex::encode(Sha256::digest(&binary));
                }
            }
            entries.push(InstalledBlock { manifest, path });
        }
        entries.sort_by(|a, b| cmp_version(&b.manifest.version, &a.manifest.version));
        entries
    }

    /// Highest-versioned installation of `name`, if any.
    pub fn find_installed(&self, name: &str) -> Option<InstalledBlock> {
        self.list_installed()
            .into_iter()
            .find(|b| b.manifest.name == name)
    }

    /// All installations of `name`, newest first.
    pub fn find_versions(&self, name: &str) -> Vec<InstalledBlock> {
        self.list_installed()
            .into_iter()
            .filter(|b| b.manifest.name == name)
            .collect()
    }

    /// Remove every installed version of `name` (binaries and sidecars).
    pub fn uninstall(&mut self, name: &str) -> Result<Vec<InstalledBlock>, String> {
        let versions = self.find_versions(name);
        if versions.is_empty() {
            return Err(format!("Block '{name}' is not installed"));
        }
        for block in &versions {
            let _ = std::fs::remove_file(&block.path);
            let _ = std::fs::remove_file(self.sidecar_path(&block.path));
        }
        log::info!(
            "BlockInstaller: uninstalled '{}' ({} version(s))",
            name,
            versions.len()
        );
        Ok(versions)
    }

    /// Back up the current highest version of `name` to a `.bak` file pair.
    pub fn backup(&mut self, name: &str) -> Result<InstalledBlock, String> {
        let block = self
            .find_installed(name)
            .ok_or_else(|| format!("Block '{name}' is not installed"))?;
        let bak = PathBuf::from(format!("{}.bak", block.path.display()));
        std::fs::copy(&block.path, &bak).map_err(|e| format!("Backup copy failed: {e}"))?;
        let sidecar = self.sidecar_path(&block.path);
        if sidecar.is_file() {
            let _ = std::fs::copy(
                &sidecar,
                PathBuf::from(format!("{}.bak", sidecar.display())),
            );
        }
        Ok(block)
    }

    /// Restore the newest `.bak` file of `name`, returning the restored block.
    pub fn rollback(&mut self, name: &str) -> Result<InstalledBlock, String> {
        let dir = self.blocks_dir.clone();
        let mut backups: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension().and_then(|e| e.to_str()) == Some("bak")
                            && p.file_name()
                                .and_then(|s| s.to_str())
                                .map(|s| s.starts_with(&format!("{name}_")))
                                .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        backups.sort();
        let backup = backups
            .pop()
            .ok_or_else(|| format!("No rollback backup found for '{name}'"))?;

        if let Some(current) = self.find_installed(name) {
            let _ = std::fs::remove_file(&current.path);
            let _ = std::fs::remove_file(self.sidecar_path(&current.path));
        }

        let binary = std::fs::read(&backup)
            .map_err(|e| format!("Failed to read backup {:?}: {e}", backup))?;
        let stem = backup
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let clean = stem
            .strip_suffix(".wasm")
            .unwrap_or(stem)
            .strip_suffix(".bin")
            .unwrap_or(stem);
        let (_, version) = parse_name_version(Path::new(clean));

        let manifest = ManifestInfo {
            name: name.to_string(),
            version,
            description: String::new(),
            author: "local".into(),
            capabilities: HashSet::new(),
            wasm_size_bytes: binary.len() as u64,
            wasm_sha256: hex::encode(Sha256::digest(&binary)),
            signature: None,
            store_url: None,
        };

        let result = self.install_from_bytes(manifest, &binary)?;
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::remove_file(PathBuf::from(format!("{}.bak", backup.display())));
        log::info!(
            "BlockInstaller: rolled back '{name}' to {}",
            result.manifest.version
        );
        Ok(result)
    }

    /// Compute pending updates by comparing installed versions with a catalog.
    pub fn check_updates(&self, catalog: &[ManifestInfo]) -> Vec<UpdateInfo> {
        let installed = self.list_installed();
        let mut updates = Vec::new();
        for block in &installed {
            let best = catalog
                .iter()
                .filter(|m| m.name == block.manifest.name)
                .max_by(|a, b| cmp_version(&a.version, &b.version));
            if let Some(cat) = best {
                if cmp_version(&cat.version, &block.manifest.version) == std::cmp::Ordering::Greater
                {
                    updates.push(UpdateInfo {
                        installed: block.manifest.clone(),
                        available: cat.clone(),
                    });
                }
            }
        }
        updates
    }
}

impl Default for BlockInstaller {
    fn default() -> Self {
        Self::new(Self::default_dir())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest(name: &str, version: &str, binary: &[u8]) -> ManifestInfo {
        ManifestInfo {
            name: name.into(),
            version: version.into(),
            description: "test".into(),
            author: "tester".into(),
            capabilities: HashSet::from(["CAP_FS_READ".to_string()]),
            wasm_size_bytes: binary.len() as u64,
            wasm_sha256: hex::encode(Sha256::digest(binary)),
            signature: None,
            store_url: None,
        }
    }

    #[test]
    fn test_cmp_version() {
        assert_eq!(cmp_version("1.0.0", "1.0.0"), std::cmp::Ordering::Equal);
        assert_eq!(cmp_version("2.0", "1.9"), std::cmp::Ordering::Greater);
        assert_eq!(cmp_version("1.0.1", "1.0.0"), std::cmp::Ordering::Greater);
        assert_eq!(cmp_version("1.10", "1.9"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_install_and_find() {
        let dir = tempfile::tempdir().unwrap();
        let mut installer = BlockInstaller::new(dir.path());
        let binary = b"wasm v1";
        let manifest = sample_manifest("net", "1.0.0", binary);
        let block = installer
            .install_from_bytes(manifest.clone(), binary)
            .unwrap();
        assert_eq!(block.manifest.version, "1.0.0");
        assert!(block.path.exists());
        assert!(installer.find_installed("net").is_some());
        assert!(installer
            .find_installed("net")
            .unwrap()
            .path
            .ends_with("net_1.0.0.wasm"));
        assert!(installer.find_installed("other").is_none());
    }

    #[test]
    fn test_install_verifies_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let mut installer = BlockInstaller::new(dir.path());
        let manifest = sample_manifest("bad", "1.0.0", b"expected");
        assert!(installer.install_from_bytes(manifest, b"tampered").is_err());
    }

    #[test]
    fn test_install_ignores_empty_hash() {
        let dir = tempfile::tempdir().unwrap();
        let mut installer = BlockInstaller::new(dir.path());
        let mut manifest = sample_manifest("ok", "1.0.0", b"data");
        manifest.wasm_sha256 = String::new();
        assert!(installer.install_from_bytes(manifest, b"data").is_ok());
    }

    #[test]
    fn test_install_sanitizes_components() {
        assert_eq!(BlockInstaller::sanitize_component("my block!"), "my_block_");
        assert_eq!(
            BlockInstaller::sanitize_component("net/../evil"),
            "net_.._evil"
        );
    }

    #[test]
    fn test_list_picks_newest_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut installer = BlockInstaller::new(dir.path());
        installer
            .install_from_bytes(sample_manifest("x", "1.0.0", b"v1"), b"v1")
            .unwrap();
        installer
            .install_from_bytes(sample_manifest("x", "2.0.0", b"v2"), b"v2")
            .unwrap();
        let all = installer.find_versions("x");
        assert_eq!(all.len(), 2);
        assert_eq!(
            installer.find_installed("x").unwrap().manifest.version,
            "2.0.0"
        );
    }

    #[test]
    fn test_uninstall_removes_all_versions() {
        let dir = tempfile::tempdir().unwrap();
        let mut installer = BlockInstaller::new(dir.path());
        installer
            .install_from_bytes(sample_manifest("y", "1.0.0", b"1"), b"1")
            .unwrap();
        installer
            .install_from_bytes(sample_manifest("y", "2.0.0", b"2"), b"2")
            .unwrap();
        installer.uninstall("y").unwrap();
        assert!(installer.find_versions("y").is_empty());
        assert!(installer.uninstall("y").is_err());
    }

    #[test]
    fn test_backup_and_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let mut installer = BlockInstaller::new(dir.path());
        installer
            .install_from_bytes(sample_manifest("z", "1.0.0", b"old"), b"old")
            .unwrap();
        installer.backup("z").unwrap();
        installer
            .install_from_bytes(sample_manifest("z", "2.0.0", b"new"), b"new")
            .unwrap();
        assert_eq!(
            installer.find_installed("z").unwrap().manifest.version,
            "2.0.0"
        );
        let restored = installer.rollback("z").unwrap();
        assert_eq!(restored.manifest.version, "1.0.0");
        let binary = std::fs::read(&restored.path).unwrap();
        assert_eq!(binary, b"old");
        assert_eq!(
            installer.find_installed("z").unwrap().manifest.version,
            "1.0.0"
        );
    }

    #[test]
    fn test_rollback_without_backup_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut installer = BlockInstaller::new(dir.path());
        installer
            .install_from_bytes(sample_manifest("nobak", "1.0.0", b"x"), b"x")
            .unwrap();
        assert!(installer.rollback("nobak").is_err());
    }

    #[test]
    fn test_check_updates() {
        let dir = tempfile::tempdir().unwrap();
        let mut installer = BlockInstaller::new(dir.path());
        installer
            .install_from_bytes(sample_manifest("app", "1.0.0", b"a"), b"a")
            .unwrap();
        installer
            .install_from_bytes(sample_manifest("uptodate", "3.0.0", b"c"), b"c")
            .unwrap();

        let catalog = vec![
            sample_manifest("app", "2.0.0", b"newer"),
            sample_manifest("uptodate", "3.0.0", b"c"),
            sample_manifest("new", "1.0.0", b"n"),
        ];
        let updates = installer.check_updates(&catalog);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].installed.name, "app");
        assert_eq!(updates[0].installed.version, "1.0.0");
        assert_eq!(updates[0].available.version, "2.0.0");
    }

    #[test]
    fn test_install_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("source.wasm");
        std::fs::write(&src, b"file-binary").unwrap();
        let mut installer = BlockInstaller::new(dir.path().join("blocks"));
        let block = installer
            .install_from_file(&src, "fromfile", "0.9.0")
            .unwrap();
        assert_eq!(block.manifest.name, "fromfile");
        assert_eq!(block.manifest.version, "0.9.0");
        assert_eq!(
            block.manifest.wasm_sha256,
            hex::encode(Sha256::digest(b"file-binary"))
        );
    }
}
