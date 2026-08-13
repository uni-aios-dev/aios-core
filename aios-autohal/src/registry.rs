use crate::manifest::DriverManifest;
use aios_security::capability::Capability;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Persisted `Fingerprint -> DriverID` mapping plus per-driver bookkeeping
/// (crash counters and capability overrides). Stored as `index.json` inside
/// the drivers directory — the VFS `AIOS://store/drivers/` backing file. The
/// same schema is what a redb table would replace on hosts that opt in.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DriverIndex {
    /// fingerprint key (`usb.046d.0825`) -> driver id.
    pub entries: HashMap<String, String>,
    /// driver id -> consecutive failure count (watchdog / self-healing).
    pub failures: HashMap<String, u32>,
    /// driver id -> user-granted capability overrides (security matrix).
    pub cap_overrides: HashMap<String, Vec<Capability>>,
}

impl DriverIndex {
    pub fn insert(&mut self, fingerprint_key: &str, driver_id: &str) {
        self.entries
            .insert(fingerprint_key.to_string(), driver_id.to_string());
    }

    pub fn get(&self, fingerprint_key: &str) -> Option<&str> {
        self.entries.get(fingerprint_key).map(String::as_str)
    }

    pub fn remove(&mut self, fingerprint_key: &str) -> Option<String> {
        self.entries.remove(fingerprint_key)
    }

    pub fn failures_for(&self, driver_id: &str) -> u32 {
        self.failures.get(driver_id).copied().unwrap_or(0)
    }

    pub fn bump_failure(&mut self, driver_id: &str) -> u32 {
        let next = self.failures_for(driver_id) + 1;
        self.failures.insert(driver_id.to_string(), next);
        next
    }

    pub fn reset_failures(&mut self, driver_id: &str) {
        self.failures.remove(driver_id);
    }

    pub fn cap_override(&self, driver_id: &str) -> Option<&[Capability]> {
        self.cap_overrides.get(driver_id).map(Vec::as_slice)
    }

    pub fn set_cap_override(&mut self, driver_id: &str, caps: Vec<Capability>) {
        self.cap_overrides.insert(driver_id.to_string(), caps);
    }

    /// Load an index from a JSON file (missing/corrupt file -> empty index).
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }
}

/// Local driver cache rooted at `AIOS://store/drivers/`. Each cached driver
/// lives in its own directory:
///
/// ```text
/// AIOS://store/drivers/
/// ├── index.json                  (DriverIndex)
/// └── driver.usb.046d.0825/
///     ├── driver.json             (DriverManifest)
///     └── driver.wasm             (WASM binary / WAT template)
/// ```
pub struct DriverStore {
    root: PathBuf,
    index: DriverIndex,
}

impl DriverStore {
    pub fn new(root: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let index = DriverIndex::load(&root.join("index.json"));
        Ok(Self { root, index })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn index(&self) -> &DriverIndex {
        &self.index
    }

    pub fn index_mut(&mut self) -> &mut DriverIndex {
        &mut self.index
    }

    pub fn save_index(&self) -> Result<(), String> {
        self.index.save(&self.root.join("index.json"))
    }

    fn driver_dir(&self, driver_id: &str) -> PathBuf {
        self.root.join(driver_id)
    }

    pub fn manifest_path(&self, driver_id: &str) -> PathBuf {
        self.driver_dir(driver_id).join("driver.json")
    }

    pub fn wasm_path(&self, driver_id: &str) -> PathBuf {
        self.driver_dir(driver_id).join("driver.wasm")
    }

    /// Persist a driver (manifest + wasm) and map a fingerprint to it.
    pub fn save_driver(
        &mut self,
        fingerprint_key: &str,
        manifest: &DriverManifest,
        wasm: &[u8],
    ) -> Result<(), String> {
        let dir = self.driver_dir(&manifest.id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = manifest.to_json_pretty()?;
        std::fs::write(self.manifest_path(&manifest.id), json).map_err(|e| e.to_string())?;
        std::fs::write(self.wasm_path(&manifest.id), wasm).map_err(|e| e.to_string())?;
        self.index.insert(fingerprint_key, &manifest.id);
        self.index.reset_failures(&manifest.id);
        self.save_index()
    }

    pub fn load_manifest(&self, driver_id: &str) -> Option<DriverManifest> {
        std::fs::read(self.manifest_path(driver_id))
            .ok()
            .and_then(|bytes| DriverManifest::from_json(&bytes).ok())
    }

    pub fn load_wasm(&self, driver_id: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.wasm_path(driver_id)).map_err(|e| e.to_string())
    }

    /// Scan every cached driver directory for a `driver.json`.
    pub fn list(&self) -> Vec<DriverManifest> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(manifest) = std::fs::read(path.join("driver.json"))
                        .ok()
                        .and_then(|b| DriverManifest::from_json(&b).ok())
                    {
                        out.push(manifest);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Remove a cached driver and every fingerprint mapping pointing at it.
    pub fn remove_driver(&mut self, driver_id: &str) -> bool {
        self.index.entries.retain(|_, v| v != driver_id);
        self.index.failures.remove(driver_id);
        self.index.cap_overrides.remove(driver_id);
        let dir = self.driver_dir(driver_id);
        let removed = std::fs::remove_dir_all(&dir).is_ok();
        let _ = self.save_index();
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::generic_fallback;

    #[test]
    fn test_store_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DriverStore::new(dir.path().join("drivers")).unwrap();
        let fb = generic_fallback();
        store
            .save_driver("usb.046d.0825", &fb.manifest, &fb.wat_bytes())
            .unwrap();

        assert_eq!(
            store.index().get("usb.046d.0825"),
            Some("driver.generic.fallback")
        );
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "driver.generic.fallback");

        let reloaded = DriverStore::new(dir.path().join("drivers")).unwrap();
        assert_eq!(
            reloaded.index().get("usb.046d.0825"),
            Some("driver.generic.fallback")
        );
        let wasm = reloaded.load_wasm("driver.generic.fallback").unwrap();
        assert_eq!(wasm, fb.wat_bytes());
    }

    #[test]
    fn test_index_failures_and_overrides() {
        let mut index = DriverIndex::default();
        assert_eq!(index.bump_failure("d"), 1);
        assert_eq!(index.bump_failure("d"), 2);
        index.reset_failures("d");
        assert_eq!(index.failures_for("d"), 0);
        index.set_cap_override("d", vec![Capability::HwAccess]);
        assert_eq!(index.cap_override("d"), Some(&[Capability::HwAccess][..]));
    }

    #[test]
    fn test_remove_driver_cleans_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DriverStore::new(dir.path().join("drivers")).unwrap();
        let fb = generic_fallback();
        store
            .save_driver("usb.046d.0825", &fb.manifest, &fb.wat_bytes())
            .unwrap();
        assert!(store.remove_driver("driver.generic.fallback"));
        assert!(store.index().get("usb.046d.0825").is_none());
        assert!(store.list().is_empty());
    }

    #[test]
    fn test_index_json_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = DriverIndex::default();
        index.insert("usb.0000.0001", "driver.x");
        index.bump_failure("driver.x");
        index.set_cap_override("driver.x", vec![Capability::FsRead]);
        let path = dir.path().join("index.json");
        index.save(&path).unwrap();
        let loaded = DriverIndex::load(&path);
        assert_eq!(loaded.get("usb.0000.0001"), Some("driver.x"));
        assert_eq!(loaded.failures_for("driver.x"), 1);
        assert_eq!(
            loaded.cap_override("driver.x"),
            Some(&[Capability::FsRead][..])
        );
    }
}
