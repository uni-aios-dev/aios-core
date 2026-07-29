use aios_core::error::Result;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct RollbackManager {
    snapshots: VecDeque<SnapshotEntry>,
    max_snapshots: usize,
    snapshot_dir: PathBuf,
}

struct SnapshotEntry {
    id: u64,
    label: String,
    timestamp: Instant,
    data: Vec<u8>,
    block_name: String,
    version: String,
}

impl RollbackManager {
    pub fn new(snapshot_dir: PathBuf, max_snapshots: usize) -> Self {
        Self {
            snapshots: VecDeque::with_capacity(max_snapshots + 1),
            max_snapshots,
            snapshot_dir,
        }
    }

    pub fn take_snapshot(
        &mut self,
        block_name: &str,
        version: &str,
        data: Vec<u8>,
        label: &str,
    ) -> u64 {
        if self.snapshots.len() >= self.max_snapshots {
            self.snapshots.pop_front();
        }

        let id = self.snapshots.len() as u64 + 1;
        self.snapshots.push_back(SnapshotEntry {
            id,
            label: label.to_string(),
            timestamp: Instant::now(),
            data,
            block_name: block_name.to_string(),
            version: version.to_string(),
        });

        log::info!("Snapshot #{id} created for '{block_name}' ({version}): {label}");
        id
    }

    pub fn rollback_to(&mut self, snapshot_id: u64) -> Result<Vec<u8>> {
        let pos = self
            .snapshots
            .iter()
            .position(|s| s.id == snapshot_id)
            .ok_or_else(|| {
                aios_core::error::AIOSException::Generic(format!(
                    "Snapshot #{snapshot_id} not found"
                ))
            })?;

        let entry = self.snapshots[pos].data.clone();
        log::info!(
            "Rollback to snapshot #{snapshot_id} ({}): {} bytes restored",
            self.snapshots[pos].label,
            entry.len()
        );

        self.snapshots.truncate(pos + 1);
        Ok(entry)
    }

    pub fn rollback_last(&mut self) -> Result<Vec<u8>> {
        let id = self
            .snapshots
            .back()
            .map(|s| s.id)
            .ok_or_else(|| {
                aios_core::error::AIOSException::Generic("No snapshots available".into())
            })?;
        self.rollback_to(id)
    }

    pub fn auto_rollback_if_needed(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(last) = self.snapshots.back() {
            if last.timestamp.elapsed() > Duration::from_secs(1) {
                let data = self.rollback_last()?;
                return Ok(Some(data));
            }
        }
        Ok(None)
    }

    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    pub fn list_labels(&self) -> Vec<(u64, String)> {
        self.snapshots
            .iter()
            .map(|s| (s.id, s.label.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_rollback_manager_create() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = RollbackManager::new(dir.path().to_path_buf(), 10);
        assert_eq!(mgr.snapshot_count(), 0);
    }

    #[test]
    fn test_take_and_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = RollbackManager::new(dir.path().to_path_buf(), 10);

        let id = mgr.take_snapshot("test-block", "1.0", vec![1, 2, 3], "initial");
        assert_eq!(id, 1);
        assert_eq!(mgr.snapshot_count(), 1);

        let restored = mgr.rollback_to(id).unwrap();
        assert_eq!(restored, vec![1, 2, 3]);
    }

    #[test]
    fn test_rollback_last() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = RollbackManager::new(dir.path().to_path_buf(), 10);

        mgr.take_snapshot("b1", "1.0", vec![10, 20], "first");
        mgr.take_snapshot("b2", "2.0", vec![30, 40], "second");

        let restored = mgr.rollback_last().unwrap();
        assert_eq!(restored, vec![30, 40]);
        assert_eq!(mgr.snapshot_count(), 1);
    }

    #[test]
    fn test_auto_rollback_not_needed_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = RollbackManager::new(dir.path().to_path_buf(), 10);
        mgr.take_snapshot("b1", "1.0", vec![1], "fresh");
        let result = mgr.auto_rollback_if_needed().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_max_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = RollbackManager::new(dir.path().to_path_buf(), 3);
        for i in 0..5 {
            mgr.take_snapshot("b", "1.0", vec![i], &format!("snap-{i}"));
        }
        assert_eq!(mgr.snapshot_count(), 3);
        let labels = mgr.list_labels();
        assert_eq!(labels[0].1, "snap-2");
    }

    #[test]
    fn test_rollback_invalid_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = RollbackManager::new(dir.path().to_path_buf(), 10);
        assert!(mgr.rollback_to(999).is_err());
    }
}
