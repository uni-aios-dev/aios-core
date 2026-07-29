use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub tags: Vec<String>,
    pub min_aios_version: String,
    pub published_at: u64,
    pub downloads: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockStatus {
    Available,
    Installed,
    UpdateAvailable,
    Deprecated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepositoryType {
    Local,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryEntry {
    pub metadata: BlockMetadata,
    pub status: BlockStatus,
    pub local_path: Option<String>,
    pub repo_type: RepositoryType,
}

pub struct BlockMarketplace {
    repositories: HashMap<String, Vec<RepositoryEntry>>,
    installed: HashMap<String, BlockMetadata>,
}

impl Default for BlockMarketplace {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockMarketplace {
    pub fn new() -> Self {
        Self {
            repositories: HashMap::new(),
            installed: HashMap::new(),
        }
    }

    pub fn add_repository(&mut self, repo_name: &str) {
        self.repositories.insert(repo_name.to_string(), Vec::new());
        log::info!("Marketplace: Repository '{}' added", repo_name);
    }

    pub fn publish_block(
        &mut self,
        repo_name: &str,
        metadata: BlockMetadata,
    ) -> Result<(), String> {
        let entries = self
            .repositories
            .get_mut(repo_name)
            .ok_or_else(|| format!("Repository '{}' not found", repo_name))?;

        let entry = RepositoryEntry {
            metadata,
            status: BlockStatus::Available,
            local_path: None,
            repo_type: RepositoryType::Remote,
        };

        log::info!(
            "Marketplace: Block '{}' v{} published to '{}'",
            entry.metadata.name,
            entry.metadata.version,
            repo_name
        );

        entries.push(entry);
        Ok(())
    }

    pub fn search(&self, query: &str) -> Vec<&BlockMetadata> {
        let lower = query.to_lowercase();
        self.repositories
            .values()
            .flatten()
            .filter(|entry| {
                entry.metadata.name.to_lowercase().contains(&lower)
                    || entry.metadata.description.to_lowercase().contains(&lower)
                    || entry
                        .metadata
                        .tags
                        .iter()
                        .any(|t| t.to_lowercase().contains(&lower))
            })
            .map(|entry| &entry.metadata)
            .collect()
    }

    pub fn search_by_tag(&self, tag: &str) -> Vec<&BlockMetadata> {
        self.repositories
            .values()
            .flatten()
            .filter(|entry| entry.metadata.tags.iter().any(|t| t == tag))
            .map(|entry| &entry.metadata)
            .collect()
    }

    pub fn install_block(
        &mut self,
        repo_name: &str,
        block_name: &str,
        block_version: &str,
        local_path: String,
    ) -> Result<BlockMetadata, String> {
        let entries = self
            .repositories
            .get_mut(repo_name)
            .ok_or_else(|| format!("Repository '{}' not found", repo_name))?;

        let entry = entries
            .iter_mut()
            .find(|e| e.metadata.name == block_name && e.metadata.version == block_version)
            .ok_or_else(|| {
                format!(
                    "Block '{}' v{} not found in repository '{}'",
                    block_name, block_version, repo_name
                )
            })?;

        entry.status = BlockStatus::Installed;
        entry.local_path = Some(local_path);
        let metadata = entry.metadata.clone();

        self.installed
            .insert(block_name.to_string(), metadata.clone());

        log::info!(
            "Marketplace: Block '{}' v{} installed from '{}'",
            block_name,
            block_version,
            repo_name
        );

        Ok(metadata)
    }

    pub fn uninstall_block(&mut self, block_name: &str) -> Result<(), String> {
        self.installed
            .remove(block_name)
            .ok_or_else(|| format!("Block '{}' not installed", block_name))?;

        for entries in self.repositories.values_mut() {
            for entry in entries.iter_mut() {
                if entry.metadata.name == block_name {
                    entry.status = BlockStatus::Available;
                    entry.local_path = None;
                }
            }
        }

        log::info!("Marketplace: Block '{}' uninstalled", block_name);
        Ok(())
    }

    pub fn check_updates(&self) -> Vec<(&str, &str, &str)> {
        let mut updates = Vec::new();
        for entries in self.repositories.values() {
            for entry in entries {
                if let Some(installed) = self.installed.get(&entry.metadata.name) {
                    if entry.metadata.version != installed.version {
                        updates.push((
                            entry.metadata.name.as_str(),
                            installed.version.as_str(),
                            entry.metadata.version.as_str(),
                        ));
                    }
                }
            }
        }
        updates
    }

    pub fn list_installed(&self) -> Vec<&BlockMetadata> {
        self.installed.values().collect()
    }

    pub fn list_repo(&self, repo_name: &str) -> Vec<&BlockMetadata> {
        self.repositories
            .get(repo_name)
            .map(|entries| entries.iter().map(|e| &e.metadata).collect())
            .unwrap_or_default()
    }

    pub fn get_block_status(&self, repo_name: &str, block_name: &str) -> Option<BlockStatus> {
        self.repositories.get(repo_name).and_then(|entries| {
            entries
                .iter()
                .find(|e| e.metadata.name == block_name)
                .map(|e| e.status)
        })
    }

    pub fn repository_names(&self) -> Vec<&str> {
        self.repositories.keys().map(|s| s.as_str()).collect()
    }

    pub fn total_blocks(&self) -> usize {
        self.repositories.values().map(|v| v.len()).sum()
    }

    pub fn total_installed(&self) -> usize {
        self.installed.len()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_metadata(
        name: &str,
        version: &str,
        description: &str,
        author: &str,
        sha256: &str,
        size_bytes: u64,
        tags: Vec<String>,
        min_aios_version: &str,
    ) -> BlockMetadata {
        let published_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        BlockMetadata {
            name: name.to_string(),
            version: version.to_string(),
            description: description.to_string(),
            author: author.to_string(),
            sha256: sha256.to_string(),
            size_bytes,
            tags,
            min_aios_version: min_aios_version.to_string(),
            published_at,
            downloads: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_metadata(name: &str, version: &str) -> BlockMetadata {
        BlockMetadata {
            name: name.to_string(),
            version: version.to_string(),
            description: format!("Test block {}", name),
            author: "test".into(),
            sha256: "abc123".into(),
            size_bytes: 1024,
            tags: vec!["test".into()],
            min_aios_version: "1.0.0".into(),
            published_at: 0,
            downloads: 0,
        }
    }

    #[test]
    fn test_marketplace_creation() {
        let mp = BlockMarketplace::new();
        assert_eq!(mp.total_blocks(), 0);
        assert_eq!(mp.total_installed(), 0);
    }

    #[test]
    fn test_add_repository() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        assert_eq!(mp.repository_names().len(), 1);
        assert!(mp.repository_names().contains(&"official"));
    }

    #[test]
    fn test_publish_block() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        let meta = make_test_metadata("compress", "1.0.0");
        assert!(mp.publish_block("official", meta).is_ok());
        assert_eq!(mp.total_blocks(), 1);
    }

    #[test]
    fn test_publish_to_nonexistent_repo() {
        let mut mp = BlockMarketplace::new();
        let meta = make_test_metadata("compress", "1.0.0");
        assert!(mp.publish_block("nonexistent", meta).is_err());
    }

    #[test]
    fn test_search_by_name() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        mp.publish_block("official", make_test_metadata("compress-zstd", "1.0.0"))
            .unwrap();
        mp.publish_block("official", make_test_metadata("network-tcp", "1.0.0"))
            .unwrap();

        let results = mp.search("compress");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "compress-zstd");
    }

    #[test]
    fn test_search_by_tag() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        let mut meta = make_test_metadata("block-a", "1.0.0");
        meta.tags = vec!["ai".into(), "inference".into()];
        mp.publish_block("official", meta).unwrap();

        let mut meta2 = make_test_metadata("block-b", "1.0.0");
        meta2.tags = vec!["storage".into()];
        mp.publish_block("official", meta2).unwrap();

        let results = mp.search_by_tag("ai");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "block-a");
    }

    #[test]
    fn test_install_block() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        mp.publish_block("official", make_test_metadata("compress", "1.0.0"))
            .unwrap();

        let result = mp.install_block("official", "compress", "1.0.0", "/blocks/compress".into());
        assert!(result.is_ok());
        assert_eq!(mp.total_installed(), 1);
        assert_eq!(
            mp.get_block_status("official", "compress"),
            Some(BlockStatus::Installed)
        );
    }

    #[test]
    fn test_install_nonexistent_block() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        let result = mp.install_block("official", "nope", "1.0.0", "/tmp".into());
        assert!(result.is_err());
    }

    #[test]
    fn test_uninstall_block() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        mp.publish_block("official", make_test_metadata("compress", "1.0.0"))
            .unwrap();
        mp.install_block("official", "compress", "1.0.0", "/blocks/compress".into())
            .unwrap();

        assert!(mp.uninstall_block("compress").is_ok());
        assert_eq!(mp.total_installed(), 0);
        assert_eq!(
            mp.get_block_status("official", "compress"),
            Some(BlockStatus::Available)
        );
    }

    #[test]
    fn test_uninstall_nonexistent() {
        let mut mp = BlockMarketplace::new();
        assert!(mp.uninstall_block("nope").is_err());
    }

    #[test]
    fn test_check_updates() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        mp.publish_block("official", make_test_metadata("compress", "1.0.0"))
            .unwrap();
        mp.install_block("official", "compress", "1.0.0", "/blocks/compress".into())
            .unwrap();

        mp.publish_block("official", make_test_metadata("compress", "2.0.0"))
            .unwrap();

        let updates = mp.check_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, "compress");
        assert_eq!(updates[0].1, "1.0.0");
        assert_eq!(updates[0].2, "2.0.0");
    }

    #[test]
    fn test_list_installed() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        mp.publish_block("official", make_test_metadata("a", "1.0.0"))
            .unwrap();
        mp.publish_block("official", make_test_metadata("b", "1.0.0"))
            .unwrap();
        mp.install_block("official", "a", "1.0.0", "/a".into())
            .unwrap();

        let installed = mp.list_installed();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "a");
    }

    #[test]
    fn test_list_repo() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        mp.publish_block("official", make_test_metadata("x", "1.0.0"))
            .unwrap();
        mp.publish_block("official", make_test_metadata("y", "1.0.0"))
            .unwrap();

        let blocks = mp.list_repo("official");
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_list_repo_nonexistent() {
        let mp = BlockMarketplace::new();
        assert!(mp.list_repo("nope").is_empty());
    }

    #[test]
    fn test_metadata_serialization() {
        let meta = make_test_metadata("compress", "1.0.0");
        let bytes = bincode::serialize(&meta).unwrap();
        let restored: BlockMetadata = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.name, "compress");
        assert_eq!(restored.version, "1.0.0");
    }

    #[test]
    fn test_search_by_description() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        mp.publish_block("official", make_test_metadata("my-block", "1.0.0"))
            .unwrap();

        let results = mp.search("test block");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_multiple_repositories() {
        let mut mp = BlockMarketplace::new();
        mp.add_repository("official");
        mp.add_repository("community");
        mp.publish_block("official", make_test_metadata("a", "1.0.0"))
            .unwrap();
        mp.publish_block("community", make_test_metadata("b", "1.0.0"))
            .unwrap();
        assert_eq!(mp.total_blocks(), 2);
        assert_eq!(mp.repository_names().len(), 2);
    }
}
