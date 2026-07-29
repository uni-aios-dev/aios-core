//! Atomic Copy-on-Write State Persistence
//!
//! Provides atomicity guarantees for block state updates and live-updates
//! with instant 1ms rollback snapshots even if hardware power is lost.

use std::path::PathBuf;

pub mod cow_storage;
pub mod recovery;
pub mod snapshot;

pub use cow_storage::CopyOnWriteStorage;
pub use recovery::RecoveryLog;
pub use snapshot::{SnapshotManager, StateSnapshot};

/// Persistence configuration
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Base directory for snapshots and recovery logs
    pub storage_dir: PathBuf,
    /// Maximum snapshot retention (in bytes)
    pub max_snapshot_size: u64,
    /// Enable compression for snapshots
    pub compress_snapshots: bool,
    /// Recovery log retention (number of entries)
    pub recovery_log_size: usize,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        PersistenceConfig {
            storage_dir: PathBuf::from(".aios-persistence"),
            max_snapshot_size: 1024 * 1024 * 100, // 100 MB
            compress_snapshots: true,
            recovery_log_size: 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = PersistenceConfig::default();
        assert_eq!(config.max_snapshot_size, 1024 * 1024 * 100);
        assert!(config.compress_snapshots);
    }
}
