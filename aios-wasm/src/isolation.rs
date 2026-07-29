use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsolationLevel {
    None,
    Process,
    Memory,
    Network,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsolationConfig {
    pub level: IsolationLevel,
    pub share_memory: bool,
    pub share_filesystem: bool,
    pub share_network: bool,
    pub resource_limits: ResourceLimits,
    pub allowed_host_calls: Vec<String>,
}

impl Default for IsolationConfig {
    fn default() -> Self {
        Self {
            level: IsolationLevel::Full,
            share_memory: false,
            share_filesystem: false,
            share_network: false,
            resource_limits: ResourceLimits::default(),
            allowed_host_calls: Vec::new(),
        }
    }
}

impl IsolationConfig {
    pub fn permissive() -> Self {
        Self {
            level: IsolationLevel::None,
            share_memory: true,
            share_filesystem: true,
            share_network: true,
            resource_limits: ResourceLimits::unlimited(),
            allowed_host_calls: vec!["log".into(), "metrics".into()],
        }
    }

    pub fn restrictive() -> Self {
        Self {
            level: IsolationLevel::Full,
            share_memory: false,
            share_filesystem: false,
            share_network: false,
            resource_limits: ResourceLimits::strict(),
            allowed_host_calls: Vec::new(),
        }
    }

    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.resource_limits = limits;
        self
    }

    pub fn allow_host_call(mut self, call: &str) -> Self {
        self.allowed_host_calls.push(call.to_string());
        self
    }

    pub fn is_host_call_allowed(&self, call: &str) -> bool {
        self.allowed_host_calls.iter().any(|c| c == call)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_memory_bytes: u64,
    pub max_cpu_time_ms: u64,
    pub max_storage_bytes: u64,
    pub max_network_bytes: u64,
    pub max_open_files: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 256 * 64 * 1024,
            max_cpu_time_ms: 30_000,
            max_storage_bytes: 100 * 1024 * 1024,
            max_network_bytes: 10 * 1024 * 1024,
            max_open_files: 32,
        }
    }
}

impl ResourceLimits {
    pub fn unlimited() -> Self {
        Self {
            max_memory_bytes: u64::MAX,
            max_cpu_time_ms: u64::MAX,
            max_storage_bytes: u64::MAX,
            max_network_bytes: u64::MAX,
            max_open_files: u32::MAX,
        }
    }

    pub fn strict() -> Self {
        Self {
            max_memory_bytes: 16 * 1024 * 1024,
            max_cpu_time_ms: 5_000,
            max_storage_bytes: 10 * 1024 * 1024,
            max_network_bytes: 0,
            max_open_files: 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IsolationBoundary {
    boundaries: HashMap<String, IsolationConfig>,
}

impl Default for IsolationBoundary {
    fn default() -> Self {
        Self::new()
    }
}

impl IsolationBoundary {
    pub fn new() -> Self {
        Self {
            boundaries: HashMap::new(),
        }
    }

    pub fn register(&mut self, block_name: &str, config: IsolationConfig) {
        log::info!(
            "WASM: Isolation boundary registered for block '{}' (level={:?})",
            block_name,
            config.level
        );
        self.boundaries.insert(block_name.to_string(), config);
    }

    pub fn get_config(&self, block_name: &str) -> Option<&IsolationConfig> {
        self.boundaries.get(block_name)
    }

    pub fn can_communicate(&self, from: &str, to: &str) -> bool {
        if from == to {
            return true;
        }
        let from_config = self.boundaries.get(from);
        let to_config = self.boundaries.get(to);
        match (from_config, to_config) {
            (Some(fc), Some(tc)) => {
                fc.level != IsolationLevel::Full && tc.level != IsolationLevel::Full
            }
            _ => false,
        }
    }

    pub fn total_boundaries(&self) -> usize {
        self.boundaries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_isolation_config() {
        let config = IsolationConfig::default();
        assert_eq!(config.level, IsolationLevel::Full);
        assert!(!config.share_memory);
        assert!(!config.share_filesystem);
        assert!(!config.share_network);
    }

    #[test]
    fn test_permissive_isolation() {
        let config = IsolationConfig::permissive();
        assert_eq!(config.level, IsolationLevel::None);
        assert!(config.share_memory);
        assert!(config.share_filesystem);
        assert!(config.share_network);
    }

    #[test]
    fn test_restrictive_isolation() {
        let config = IsolationConfig::restrictive();
        assert_eq!(config.level, IsolationLevel::Full);
        assert!(!config.share_memory);
    }

    #[test]
    fn test_resource_limits_defaults() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.max_memory_bytes, 256 * 64 * 1024);
        assert_eq!(limits.max_cpu_time_ms, 30_000);
        assert_eq!(limits.max_open_files, 32);
    }

    #[test]
    fn test_resource_limits_strict() {
        let limits = ResourceLimits::strict();
        assert_eq!(limits.max_memory_bytes, 16 * 1024 * 1024);
        assert_eq!(limits.max_cpu_time_ms, 5_000);
        assert_eq!(limits.max_network_bytes, 0);
        assert_eq!(limits.max_open_files, 4);
    }

    #[test]
    fn test_resource_limits_unlimited() {
        let limits = ResourceLimits::unlimited();
        assert_eq!(limits.max_memory_bytes, u64::MAX);
        assert_eq!(limits.max_open_files, u32::MAX);
    }

    #[test]
    fn test_isolation_boundary_same_block() {
        let mut boundary = IsolationBoundary::new();
        boundary.register("block_a", IsolationConfig::restrictive());
        assert!(boundary.can_communicate("block_a", "block_a"));
    }

    #[test]
    fn test_isolation_boundary_different_blocks_full_isolation() {
        let mut boundary = IsolationBoundary::new();
        boundary.register("block_a", IsolationConfig::restrictive());
        boundary.register("block_b", IsolationConfig::restrictive());
        assert!(!boundary.can_communicate("block_a", "block_b"));
    }

    #[test]
    fn test_isolation_boundary_permissive_communication() {
        let mut boundary = IsolationBoundary::new();
        boundary.register("block_a", IsolationConfig::permissive());
        boundary.register("block_b", IsolationConfig::permissive());
        assert!(boundary.can_communicate("block_a", "block_b"));
    }

    #[test]
    fn test_isolation_boundary_mixed_communication() {
        let mut boundary = IsolationBoundary::new();
        boundary.register("block_a", IsolationConfig::permissive());
        boundary.register("block_b", IsolationConfig::restrictive());
        assert!(!boundary.can_communicate("block_a", "block_b"));
    }

    #[test]
    fn test_isolation_boundary_unregistered_block() {
        let boundary = IsolationBoundary::new();
        assert!(!boundary.can_communicate("block_a", "block_b"));
        assert!(boundary.get_config("block_a").is_none());
    }

    #[test]
    fn test_host_call_allowed() {
        let config = IsolationConfig::default().allow_host_call("log");
        assert!(config.is_host_call_allowed("log"));
        assert!(!config.is_host_call_allowed("network"));
    }

    #[test]
    fn test_isolation_config_serialization() {
        let config = IsolationConfig::default();
        let bytes = bincode::serialize(&config).unwrap();
        let restored: IsolationConfig = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.level, IsolationLevel::Full);
    }

    #[test]
    fn test_boundary_total_count() {
        let mut boundary = IsolationBoundary::new();
        boundary.register("a", IsolationConfig::default());
        boundary.register("b", IsolationConfig::default());
        boundary.register("c", IsolationConfig::default());
        assert_eq!(boundary.total_boundaries(), 3);
    }

    #[test]
    fn test_boundary_not_registered_no_communication() {
        let boundary = IsolationBoundary::new();
        assert!(!boundary.can_communicate("unregistered_a", "unregistered_b"));
    }
}
