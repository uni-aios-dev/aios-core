use crate::manifest::ManifestInfo;
use std::collections::HashMap;

pub struct StoreRegistry {
    blocks: HashMap<String, ManifestInfo>,
}

impl StoreRegistry {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
        }
    }

    pub fn register(&mut self, manifest: ManifestInfo) -> Result<(), String> {
        let key = format!("{}@{}", manifest.name, manifest.version);
        if self.blocks.contains_key(&key) {
            return Err(format!("Block '{key}' already registered"));
        }
        self.blocks.insert(key, manifest);
        Ok(())
    }

    pub fn get(&self, name: &str, version: &str) -> Option<&ManifestInfo> {
        let key = format!("{name}@{version}");
        self.blocks.get(&key)
    }

    pub fn find_all(&self, name: &str) -> Vec<&ManifestInfo> {
        self.blocks
            .iter()
            .filter(|(k, _)| k.starts_with(&format!("{name}@")))
            .map(|(_, v)| v)
            .collect()
    }

    pub fn list(&self) -> Vec<&ManifestInfo> {
        self.blocks.values().collect()
    }

    pub fn count(&self) -> usize {
        self.blocks.len()
    }

    pub fn unregister(&mut self, name: &str, version: &str) -> Option<ManifestInfo> {
        let key = format!("{name}@{version}");
        self.blocks.remove(&key)
    }
}

impl Default for StoreRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::HashSet;

    #[test]
    fn test_registry_empty() {
        let reg = StoreRegistry::new();
        assert_eq!(reg.count(), 0);
    }

    #[test]
    fn test_register_and_get() {
        let mut reg = StoreRegistry::new();
        let m = sample_manifest("browser", "1.0");
        reg.register(m).unwrap();
        assert_eq!(reg.count(), 1);
        assert!(reg.get("browser", "1.0").is_some());
    }

    #[test]
    fn test_register_duplicate() {
        let mut reg = StoreRegistry::new();
        let m1 = sample_manifest("browser", "1.0");
        let m2 = sample_manifest("browser", "1.0");
        reg.register(m1).unwrap();
        assert!(reg.register(m2).is_err());
    }

    #[test]
    fn test_find_all_versions() {
        let mut reg = StoreRegistry::new();
        reg.register(sample_manifest("browser", "1.0")).unwrap();
        reg.register(sample_manifest("browser", "2.0")).unwrap();
        reg.register(sample_manifest("network", "1.0")).unwrap();
        assert_eq!(reg.find_all("browser").len(), 2);
    }

    #[test]
    fn test_unregister() {
        let mut reg = StoreRegistry::new();
        reg.register(sample_manifest("test", "1.0")).unwrap();
        assert!(reg.unregister("test", "1.0").is_some());
        assert_eq!(reg.count(), 0);
    }

    fn sample_manifest(name: &str, version: &str) -> ManifestInfo {
        ManifestInfo {
            name: name.into(),
            version: version.into(),
            description: "test".into(),
            author: "test".into(),
            capabilities: HashSet::new(),
            wasm_size_bytes: 100,
            wasm_sha256: hex::encode(Sha256::digest(b"test")),
            signature: None,
            store_url: None,
        }
    }
}
