use crate::loader::BlockLoader;
use crate::registry::BlockRegistry;
use aios_core::block::BlockId;
use aios_core::crypto;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct HotReloadConfig {
    pub watch_dir: PathBuf,
    pub poll_interval_ms: u64,
    pub auto_activate: bool,
}

impl Default for HotReloadConfig {
    fn default() -> Self {
        Self {
            watch_dir: PathBuf::from("blocks"),
            poll_interval_ms: 1000,
            auto_activate: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrackedFile {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub sha256: [u8; 32],
    pub loaded_id: Option<BlockId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadEvent {
    NewBlock { path: PathBuf, block_id: BlockId },
    UpdatedBlock { path: PathBuf, block_id: BlockId },
    RemovedBlock { path: PathBuf, block_id: BlockId },
    Error { path: PathBuf, error: String },
    NoChange,
}

pub struct HotReloader {
    config: HotReloadConfig,
    tracked: HashMap<PathBuf, TrackedFile>,
    event_log: Vec<ReloadEvent>,
}

impl HotReloader {
    pub fn new(config: HotReloadConfig) -> Self {
        Self {
            config,
            tracked: HashMap::new(),
            event_log: Vec::new(),
        }
    }

    pub fn with_watch_dir(path: impl Into<PathBuf>) -> Self {
        Self::new(HotReloadConfig {
            watch_dir: path.into(),
            ..Default::default()
        })
    }

    pub fn config(&self) -> &HotReloadConfig {
        &self.config
    }

    pub fn event_log(&self) -> &[ReloadEvent] {
        &self.event_log
    }

    pub fn tracked_files(&self) -> &HashMap<PathBuf, TrackedFile> {
        &self.tracked
    }

    pub fn scan_and_reload(&mut self, registry: &mut BlockRegistry) -> Vec<ReloadEvent> {
        let mut events = Vec::new();

        if !self.config.watch_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&self.config.watch_dir) {
                let event = ReloadEvent::Error {
                    path: self.config.watch_dir.clone(),
                    error: e.to_string(),
                };
                events.push(event.clone());
                self.event_log.push(event);
            }
            return events;
        }

        let current_files = self.scan_directory();
        let mut to_remove: Vec<PathBuf> = Vec::new();

        for (path, tracked) in &self.tracked {
            if !current_files.contains(path) {
                if let Some(block_id) = tracked.loaded_id {
                    let event = ReloadEvent::RemovedBlock {
                        path: path.clone(),
                        block_id,
                    };
                    events.push(event.clone());
                    self.event_log.push(event);
                }
                to_remove.push(path.clone());
            }
        }
        for path in to_remove {
            self.tracked.remove(&path);
        }

        for file_path in &current_files {
            let meta = match std::fs::metadata(file_path) {
                Ok(m) => m,
                Err(e) => {
                    let event = ReloadEvent::Error {
                        path: file_path.clone(),
                        error: e.to_string(),
                    };
                    events.push(event.clone());
                    self.event_log.push(event);
                    continue;
                }
            };

            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

            if let Some(tracked) = self.tracked.get(file_path) {
                if tracked.modified >= modified {
                    continue;
                }
            }

            let binary = match std::fs::read(file_path) {
                Ok(b) => b,
                Err(e) => {
                    let event = ReloadEvent::Error {
                        path: file_path.clone(),
                        error: e.to_string(),
                    };
                    events.push(event.clone());
                    self.event_log.push(event);
                    continue;
                }
            };

            let sha256 = crypto::compute_sha256_bytes(&binary);
            let name = file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            let version = Self::extract_version_from_path(file_path);

            if let Some(tracked) = self.tracked.get(file_path) {
                if tracked.sha256 == sha256 {
                    continue;
                }

                if let Some(old_id) = tracked.loaded_id {
                    let _ = registry.unload_block(old_id);
                }

                match BlockLoader::load_from_binary(registry, name, &version, binary.clone()) {
                    Ok(manifest) => {
                        let event = ReloadEvent::UpdatedBlock {
                            path: file_path.clone(),
                            block_id: manifest.id,
                        };
                        events.push(event.clone());
                        self.event_log.push(event);
                        self.tracked.insert(
                            file_path.clone(),
                            TrackedFile {
                                path: file_path.clone(),
                                modified,
                                sha256,
                                loaded_id: Some(manifest.id),
                            },
                        );
                    }
                    Err(e) => {
                        let event = ReloadEvent::Error {
                            path: file_path.clone(),
                            error: e.to_string(),
                        };
                        events.push(event.clone());
                        self.event_log.push(event);
                    }
                }
            } else {
                match BlockLoader::load_from_binary(registry, name, &version, binary.clone()) {
                    Ok(manifest) => {
                        let event = ReloadEvent::NewBlock {
                            path: file_path.clone(),
                            block_id: manifest.id,
                        };
                        events.push(event.clone());
                        self.event_log.push(event);
                        self.tracked.insert(
                            file_path.clone(),
                            TrackedFile {
                                path: file_path.clone(),
                                modified,
                                sha256,
                                loaded_id: Some(manifest.id),
                            },
                        );
                    }
                    Err(e) => {
                        let event = ReloadEvent::Error {
                            path: file_path.clone(),
                            error: e.to_string(),
                        };
                        events.push(event.clone());
                        self.event_log.push(event);
                    }
                }
            }
        }

        if events.is_empty() {
            vec![ReloadEvent::NoChange]
        } else {
            events
        }
    }

    fn scan_directory(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.config.watch_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if ext == "bin" || ext == "aib" {
                            files.push(path);
                        }
                    }
                }
            }
        }
        files
    }

    fn extract_version_from_path(path: &Path) -> String {
        if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
            let parts: Vec<&str> = name.split('_').collect();
            if parts.len() > 1 {
                return parts.last().unwrap().to_string();
            }
        }
        "1.0.0".into()
    }

    pub fn register_existing(&mut self, path: PathBuf, block_id: BlockId, sha256: [u8; 32]) {
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        self.tracked.insert(
            path.clone(),
            TrackedFile {
                path,
                modified,
                sha256,
                loaded_id: Some(block_id),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hot_reloader_creation() {
        let reloader = HotReloader::new(HotReloadConfig {
            watch_dir: PathBuf::from("/tmp/test_blocks"),
            poll_interval_ms: 500,
            auto_activate: true,
        });
        assert_eq!(reloader.config().poll_interval_ms, 500);
        assert!(reloader.tracked_files().is_empty());
    }

    #[test]
    fn test_hot_reloader_with_watch_dir() {
        let reloader = HotReloader::with_watch_dir("/tmp/blocks");
        assert_eq!(reloader.config().watch_dir, PathBuf::from("/tmp/blocks"));
    }

    #[test]
    fn test_scan_empty_directory() {
        let dir = std::env::temp_dir().join("aios_test_empty_dir");
        let _ = std::fs::create_dir_all(&dir);

        let mut reloader = HotReloader::with_watch_dir(&dir);
        let mut registry = BlockRegistry::new();
        let events = reloader.scan_and_reload(&mut registry);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ReloadEvent::NoChange));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scan_directory_finds_bin_files() {
        let dir = std::env::temp_dir().join("aios_test_bin_dir");
        let _ = std::fs::create_dir_all(&dir);

        let bin_file = dir.join("test_module.bin");
        let _ = std::fs::write(&bin_file, b"test binary data");

        let txt_file = dir.join("readme.txt");
        let _ = std::fs::write(&txt_file, b"not a binary");

        let reloader = HotReloader::with_watch_dir(&dir);
        let files = reloader.scan_directory();

        assert_eq!(files.len(), 1);
        assert!(files[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("test_module"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_extract_version_from_path() {
        let p = PathBuf::from("blocks/my_module_v2.1.0.bin");
        assert_eq!(HotReloader::extract_version_from_path(&p), "v2.1.0");

        let p2 = PathBuf::from("blocks/simple.bin");
        assert_eq!(HotReloader::extract_version_from_path(&p2), "1.0.0");
    }

    #[test]
    fn test_register_existing() {
        let dir = std::env::temp_dir().join("aios_test_register");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test.bin");
        let _ = std::fs::write(&file, b"data");

        let mut reloader = HotReloader::with_watch_dir(&dir);
        let sha = crypto::compute_sha256_bytes(b"data");
        reloader.register_existing(file.clone(), BlockId::new(1), sha);

        assert_eq!(reloader.tracked_files().len(), 1);
        assert!(reloader.tracked_files().contains_key(&file));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_new_file_and_load() {
        let dir = std::env::temp_dir().join("aios_test_new_file");
        let _ = std::fs::create_dir_all(&dir);

        let mut reloader = HotReloader::with_watch_dir(&dir);
        let mut registry = BlockRegistry::new();

        let events1 = reloader.scan_and_reload(&mut registry);
        assert!(matches!(events1[0], ReloadEvent::NoChange));

        let bin_file = dir.join("hot_block.bin");
        let _ = std::fs::write(&bin_file, b"hot reload binary");

        let events2 = reloader.scan_and_reload(&mut registry);
        assert_eq!(events2.len(), 1);
        assert!(
            matches!(&events2[0], ReloadEvent::NewBlock { block_id, .. } if registry.get(*block_id).is_ok())
        );

        assert_eq!(registry.count(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_file_update() {
        let dir = std::env::temp_dir().join("aios_test_update");
        let _ = std::fs::create_dir_all(&dir);

        let bin_file = dir.join("update_block.bin");
        let _ = std::fs::write(&bin_file, b"version 1");

        let mut reloader = HotReloader::with_watch_dir(&dir);
        let mut registry = BlockRegistry::new();

        let events1 = reloader.scan_and_reload(&mut registry);
        assert!(matches!(&events1[0], ReloadEvent::NewBlock { .. }));

        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = std::fs::write(&bin_file, b"version 2");

        let events2 = reloader.scan_and_reload(&mut registry);
        assert!(matches!(&events2[0], ReloadEvent::UpdatedBlock { .. }));
        assert_eq!(registry.count(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nonexistent_watch_dir_creates_it() {
        let dir = std::env::temp_dir().join("aios_test_create_dir");
        let _ = std::fs::remove_dir_all(&dir);

        let mut reloader = HotReloader::with_watch_dir(&dir);
        let mut registry = BlockRegistry::new();

        let _ = reloader.scan_and_reload(&mut registry);
        assert!(dir.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_event_log_accumulates() {
        let dir = std::env::temp_dir().join("aios_test_event_log");
        let _ = std::fs::create_dir_all(&dir);

        let mut reloader = HotReloader::with_watch_dir(&dir);
        let mut registry = BlockRegistry::new();

        reloader.scan_and_reload(&mut registry);

        let bin_file = dir.join("block1.bin");
        let _ = std::fs::write(&bin_file, b"block 1");
        reloader.scan_and_reload(&mut registry);

        assert!(reloader.event_log().len() >= 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
