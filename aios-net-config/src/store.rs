use crate::config::NetworkConfig;
use std::path::{Path, PathBuf};

/// Persists a [`NetworkConfig`] as a JSON file on disk.
///
/// Default location honors the `AIOS_DATA_DIR` environment variable and falls
/// back to a `network.json` file next to the working directory.
pub struct NetworkConfigStore {
    path: PathBuf,
}

impl NetworkConfigStore {
    /// Create a store bound to `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Default configuration path for the current machine.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("AIOS_DATA_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("network.json")
    }

    /// Path this store is bound to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the configuration from disk, returning `None` when no file exists.
    pub fn load(&self) -> Result<Option<NetworkConfig>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(data) => {
                let config = NetworkConfig::from_json(&data)?;
                log::info!("NetworkConfigStore: loaded config from {:?}", self.path);
                Ok(Some(config))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Failed to read {:?}: {e}", self.path)),
        }
    }

    /// Load the configuration or fall back to `default` when no file exists.
    pub fn load_or(&self, default: NetworkConfig) -> Result<NetworkConfig, String> {
        Ok(self.load()?.unwrap_or(default))
    }

    /// Save the configuration atomically (write temp file, then rename).
    pub fn save(&self, config: &NetworkConfig) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create {:?}: {e}", parent))?;
            }
        }
        let json = config.to_json();
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("Failed to write {:?}: {e}", tmp))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("Failed to rename {:?}: {e}", self.path))?;
        log::info!("NetworkConfigStore: saved config to {:?}", self.path);
        Ok(())
    }

    /// Remove the configuration file from disk.
    pub fn delete(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                log::info!("NetworkConfigStore: removed config {:?}", self.path);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Failed to remove {:?}: {e}", self.path)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyProtocol;

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = NetworkConfigStore::new(dir.path().join("net.json"));
        let mut config = NetworkConfig::default();
        config.hostname = "roundtrip-host".into();
        config.proxy = Some(crate::config::ProxyConfig {
            protocol: ProxyProtocol::Https,
            host: "secure.proxy".into(),
            port: 443,
            username: None,
            password: None,
        });
        store.save(&config).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn test_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = NetworkConfigStore::new(dir.path().join("absent.json"));
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn test_load_or_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = NetworkConfigStore::new(dir.path().join("absent.json"));
        let config = store.load_or(NetworkConfig::default()).unwrap();
        assert_eq!(config.hostname, "aios-host");
    }

    #[test]
    fn test_delete_missing_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let store = NetworkConfigStore::new(dir.path().join("nope.json"));
        assert!(store.delete().is_ok());
    }

    #[test]
    fn test_delete_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = NetworkConfigStore::new(dir.path().join("net.json"));
        store.save(&NetworkConfig::default()).unwrap();
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn test_default_path_honors_env() {
        std::env::set_var("AIOS_DATA_DIR", "C:/tmp/aios-test-data");
        let path = NetworkConfigStore::default_path();
        std::env::remove_var("AIOS_DATA_DIR");
        assert_eq!(path, PathBuf::from("C:/tmp/aios-test-data/network.json"));
    }
}
