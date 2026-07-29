//! State snapshot management for atomic persistence

use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// State snapshot metadata and data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Snapshot ID (typically block_id + timestamp)
    pub id: String,
    /// Timestamp (Unix milliseconds)
    pub timestamp_ms: u64,
    /// State data (binary blob)
    pub data: Vec<u8>,
    /// SHA-256 checksum for integrity
    pub checksum: [u8; 32],
    /// Size in bytes
    pub size_bytes: usize,
}

impl StateSnapshot {
    /// Create new snapshot
    pub fn new(id: String, data: Vec<u8>, checksum: [u8; 32]) -> Self {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let size_bytes = data.len();

        StateSnapshot {
            id,
            timestamp_ms,
            data,
            checksum,
            size_bytes,
        }
    }

    /// Verify snapshot integrity
    pub fn verify(&self) -> bool {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&self.data);
        let result = hasher.finalize();
        result[..] == self.checksum
    }

    /// Serialize snapshot to bytes
    pub fn serialize(&self) -> Result<Vec<u8>> {
        bincode::serialize(self).map_err(|e| AIOSException::SerializationError(e.to_string()))
    }

    /// Deserialize snapshot from bytes
    pub fn deserialize(data: &[u8]) -> Result<Self> {
        bincode::deserialize(data).map_err(|e| AIOSException::SerializationError(e.to_string()))
    }
}

/// Snapshot manager for atomic state persistence
pub struct SnapshotManager {
    storage_dir: PathBuf,
    max_size: u64,
}

impl SnapshotManager {
    /// Create snapshot manager
    pub fn new(storage_dir: PathBuf, max_size: u64) -> Result<Self> {
        if !storage_dir.exists() {
            fs::create_dir_all(&storage_dir).map_err(|e| AIOSException::IPCError(e.to_string()))?;
        }

        Ok(SnapshotManager {
            storage_dir,
            max_size,
        })
    }

    /// Check if saving a snapshot would exceed max storage capacity
    pub fn would_exceed_capacity(&self, snapshot_size: u64) -> Result<bool> {
        let current = self.storage_usage()?;
        Ok(current + snapshot_size > self.max_size)
    }

    /// Get configured maximum storage size in bytes
    pub fn max_capacity(&self) -> u64 {
        self.max_size
    }

    /// Save snapshot atomically (write to temp, then rename)
    pub fn save(&self, snapshot: &StateSnapshot) -> Result<PathBuf> {
        let metadata = bincode::serialize(snapshot)
            .map_err(|e| AIOSException::SerializationError(e.to_string()))?;

        // Write to shadow file first
        let temp_path = self.storage_dir.join(format!("{}.tmp", snapshot.id));
        let final_path = self.storage_dir.join(&snapshot.id);

        fs::write(&temp_path, &metadata).map_err(|e| AIOSException::IPCError(e.to_string()))?;

        // Atomic rename (power-safe on most filesystems)
        fs::rename(&temp_path, &final_path).map_err(|e| AIOSException::IPCError(e.to_string()))?;

        Ok(final_path)
    }

    /// Load snapshot
    pub fn load(&self, snapshot_id: &str) -> Result<StateSnapshot> {
        let path = self.storage_dir.join(snapshot_id);
        if !path.exists() {
            return Err(AIOSException::Generic(format!(
                "Snapshot not found: {}",
                snapshot_id
            )));
        }

        let data = fs::read(&path).map_err(|e| AIOSException::IPCError(e.to_string()))?;

        StateSnapshot::deserialize(&data)
    }

    /// List all snapshots
    pub fn list_snapshots(&self) -> Result<Vec<String>> {
        let mut snapshots = Vec::new();
        for entry in
            fs::read_dir(&self.storage_dir).map_err(|e| AIOSException::IPCError(e.to_string()))?
        {
            let entry = entry.map_err(|e| AIOSException::IPCError(e.to_string()))?;
            let path = entry.path();
            if path.is_file() && !path.to_string_lossy().ends_with(".tmp") {
                if let Some(name) = path.file_name() {
                    snapshots.push(name.to_string_lossy().to_string());
                }
            }
        }
        Ok(snapshots)
    }

    /// Delete snapshot
    pub fn delete(&self, snapshot_id: &str) -> Result<()> {
        let path = self.storage_dir.join(snapshot_id);
        fs::remove_file(&path).map_err(|e| AIOSException::IPCError(e.to_string()))
    }

    /// Get storage usage
    pub fn storage_usage(&self) -> Result<u64> {
        let mut total = 0u64;
        for entry in
            fs::read_dir(&self.storage_dir).map_err(|e| AIOSException::IPCError(e.to_string()))?
        {
            let entry = entry.map_err(|e| AIOSException::IPCError(e.to_string()))?;
            let metadata = entry
                .metadata()
                .map_err(|e| AIOSException::IPCError(e.to_string()))?;
            if metadata.is_file() {
                total += metadata.len();
            }
        }
        Ok(total)
    }

    /// Clean old snapshots (FIFO)
    pub fn cleanup_oldest(&self, keep_count: usize) -> Result<usize> {
        let mut snapshots = self.list_snapshots()?;
        snapshots.sort();

        let mut deleted = 0;
        while snapshots.len() > keep_count {
            if let Some(oldest) = snapshots.first() {
                self.delete(oldest)?;
                deleted += 1;
            }
            snapshots.remove(0);
        }

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_creation() {
        let data = vec![1, 2, 3, 4, 5];
        let checksum = [0u8; 32];
        let snapshot = StateSnapshot::new("test".to_string(), data, checksum);

        assert_eq!(snapshot.id, "test");
        assert_eq!(snapshot.size_bytes, 5);
        assert!(snapshot.timestamp_ms > 0);
    }

    #[test]
    fn test_snapshot_serialization() {
        let data = b"test data".to_vec();
        let checksum = [0u8; 32];
        let snapshot = StateSnapshot::new("test".to_string(), data, checksum);

        let serialized = snapshot.serialize().unwrap();
        let deserialized = StateSnapshot::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.id, snapshot.id);
        assert_eq!(deserialized.size_bytes, snapshot.size_bytes);
    }

    #[test]
    fn test_snapshot_manager() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = SnapshotManager::new(temp_dir.path().to_path_buf(), 1024 * 1024).unwrap();

        let snapshot = StateSnapshot::new("snapshot_1".to_string(), vec![1, 2, 3], [0u8; 32]);

        manager.save(&snapshot).unwrap();
        let loaded = manager.load("snapshot_1").unwrap();
        assert_eq!(loaded.id, snapshot.id);

        let snapshots = manager.list_snapshots().unwrap();
        assert_eq!(snapshots.len(), 1);
    }
}
