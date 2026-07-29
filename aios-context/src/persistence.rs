use crate::stability::StabilityStore;
use crate::telemetry::{TelemetryEntry, TelemetryStore};
use crate::workflow::WorkflowStore;
use log::{info, warn};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;

const TELEMETRY_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("telemetry");
const WORKFLOW_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("workflows");
const STABILITY_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("stability");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedTelemetryEntry {
    pub metric_name: String,
    pub value: f64,
    pub ram_used_mb: u64,
    pub timestamp_ms: u64,
    pub block_id: Option<u32>,
    pub process_name: Option<String>,
}

impl From<&TelemetryEntry> for PersistedTelemetryEntry {
    fn from(e: &TelemetryEntry) -> Self {
        Self {
            metric_name: e.metric_name.clone(),
            value: e.value,
            ram_used_mb: e.ram_used_mb,
            timestamp_ms: e.timestamp_ms,
            block_id: e.block_id,
            process_name: e.process_name.clone(),
        }
    }
}

impl From<PersistedTelemetryEntry> for TelemetryEntry {
    fn from(e: PersistedTelemetryEntry) -> Self {
        Self {
            metric_name: e.metric_name,
            value: e.value,
            ram_used_mb: e.ram_used_mb,
            timestamp_ms: e.timestamp_ms,
            block_id: e.block_id,
            process_name: e.process_name,
        }
    }
}

pub struct PersistentStore {
    db: Option<Database>,
    db_path: String,
}

impl PersistentStore {
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        let path_str = db_path.as_ref().to_string_lossy().to_string();
        match Database::create(&db_path) {
            Ok(db) => {
                info!("PersistentStore: opened database at {}", path_str);
                Self {
                    db: Some(db),
                    db_path: path_str,
                }
            }
            Err(e) => {
                warn!(
                    "PersistentStore: failed to open database at {}: {}",
                    path_str, e
                );
                Self {
                    db: None,
                    db_path: path_str,
                }
            }
        }
    }

    pub fn is_available(&self) -> bool {
        self.db.is_some()
    }

    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    pub fn save_telemetry(&self, entries: &[TelemetryEntry]) -> Result<usize, String> {
        let db = self.db.as_ref().ok_or("Database not available")?;
        let write_txn = db.begin_write().map_err(|e| e.to_string())?;
        {
            let mut table = write_txn
                .open_table(TELEMETRY_TABLE)
                .map_err(|e| e.to_string())?;
            for (i, entry) in entries.iter().enumerate() {
                let persisted = PersistedTelemetryEntry::from(entry);
                let bytes = bincode::serialize(&persisted).map_err(|e| e.to_string())?;
                let key = (i as u64) + 1;
                table
                    .insert(key, bytes.as_slice())
                    .map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        }
        .map_err(|e: String| e)?;
        write_txn.commit().map_err(|e| e.to_string())?;
        info!("PersistentStore: saved {} telemetry entries", entries.len());
        Ok(entries.len())
    }

    pub fn load_telemetry(&self) -> Result<Vec<TelemetryEntry>, String> {
        let db = self.db.as_ref().ok_or("Database not available")?;
        let read_txn = db.begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(TELEMETRY_TABLE) {
            Ok(t) => t,
            Err(_) => return Ok(Vec::new()),
        };
        let mut entries = Vec::new();
        let iter = table.iter().map_err(|e| e.to_string())?;
        for result in iter {
            let (_, value) = result.map_err(|e| e.to_string())?;
            let bytes = value.value();
            if let Ok(persisted) = bincode::deserialize::<PersistedTelemetryEntry>(bytes) {
                entries.push(TelemetryEntry::from(persisted));
            }
        }
        Ok(entries)
    }

    pub fn save_workflows(&self, store: &WorkflowStore) -> Result<usize, String> {
        let db = self.db.as_ref().ok_or("Database not available")?;
        let write_txn = db.begin_write().map_err(|e| e.to_string())?;
        {
            let mut table = write_txn
                .open_table(WORKFLOW_TABLE)
                .map_err(|e| e.to_string())?;
            for profile in store.profiles.values() {
                let bytes = bincode::serialize(profile).map_err(|e| e.to_string())?;
                table
                    .insert(profile.name.as_str(), bytes.as_slice())
                    .map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        }
        .map_err(|e: String| e)?;
        write_txn.commit().map_err(|e| e.to_string())?;
        Ok(store.profiles.len())
    }

    pub fn save_stability(&self, store: &StabilityStore) -> Result<usize, String> {
        let db = self.db.as_ref().ok_or("Database not available")?;
        let write_txn = db.begin_write().map_err(|e| e.to_string())?;
        {
            let mut table = write_txn
                .open_table(STABILITY_TABLE)
                .map_err(|e| e.to_string())?;
            for score in &store.scores {
                let key = format!("{}:{}", score.block_name, score.binary_version);
                let bytes = bincode::serialize(score).map_err(|e| e.to_string())?;
                table
                    .insert(key.as_str(), bytes.as_slice())
                    .map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        }
        .map_err(|e: String| e)?;
        write_txn.commit().map_err(|e| e.to_string())?;
        Ok(store.scores.len())
    }

    pub fn save_version(&self, version: &str) -> Result<(), String> {
        let db = self.db.as_ref().ok_or("Database not available")?;
        let write_txn = db.begin_write().map_err(|e| e.to_string())?;
        {
            let mut table = write_txn
                .open_table(META_TABLE)
                .map_err(|e| e.to_string())?;
            table
                .insert("db_version", version.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        write_txn.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn load_version(&self) -> Option<String> {
        let db = self.db.as_ref()?;
        let read_txn = db.begin_read().ok()?;
        let table = read_txn.open_table(META_TABLE).ok()?;
        let val = table.get("db_version").ok()??;
        Some(String::from_utf8_lossy(val.value()).to_string())
    }

    pub fn compact(&mut self) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            db.compact().map_err(|e| e.to_string())?;
            info!("PersistentStore: database compacted");
        }
        Ok(())
    }

    pub fn save_all(
        &self,
        telemetry: &TelemetryStore,
        workflows: &WorkflowStore,
        stability: &StabilityStore,
    ) -> Result<PersistSummary, String> {
        let t = self.save_telemetry(&telemetry.entries)?;
        let w = self.save_workflows(workflows)?;
        let s = self.save_stability(stability)?;
        self.save_version("0.5.0")?;
        Ok(PersistSummary {
            telemetry_entries: t,
            workflow_entries: w,
            stability_entries: s,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistSummary {
    pub telemetry_entries: usize,
    pub workflow_entries: usize,
    pub stability_entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stability::StabilityScore;
    use std::path::PathBuf;

    fn test_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("aios_test_{}.redb", name))
    }

    fn cleanup(path: &PathBuf) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}.prep", path.display()));
    }

    #[test]
    fn test_persistent_store_creation() {
        let path = test_db_path("create");
        let store = PersistentStore::new(&path);
        assert!(store.is_available());
        cleanup(&path);
    }

    #[test]
    fn test_save_and_load_telemetry() {
        let path = test_db_path("telemetry");
        let store = PersistentStore::new(&path);

        let entries = vec![
            TelemetryEntry::new("cpu", 50.0, 1024),
            TelemetryEntry::new("cpu", 60.0, 2048),
        ];
        let count = store.save_telemetry(&entries).unwrap();
        assert_eq!(count, 2);

        cleanup(&path);
    }

    #[test]
    fn test_save_version() {
        let path = test_db_path("version");
        let store = PersistentStore::new(&path);

        store.save_version("0.5.0").unwrap();
        assert_eq!(store.load_version(), Some("0.5.0".into()));

        cleanup(&path);
    }

    #[test]
    fn test_compact() {
        let path = test_db_path("compact");
        let mut store = PersistentStore::new(&path);
        store.compact().unwrap();
        cleanup(&path);
    }

    #[test]
    fn test_unavailable_store() {
        let store = PersistentStore {
            db: None,
            db_path: "/nonexistent".into(),
        };
        assert!(!store.is_available());
        assert!(store.save_telemetry(&vec![]).is_err());
        assert!(store.load_telemetry().is_err());
    }

    #[test]
    fn test_save_all() {
        let path = test_db_path("saveall");
        let store = PersistentStore::new(&path);

        let mut telemetry = TelemetryStore::new();
        telemetry.record(TelemetryEntry::new("cpu", 75.0, 4096));

        let mut workflows = WorkflowStore::new();
        workflows.record("editing".into(), vec!["process_a".into()]);

        let mut stability = StabilityStore::new();
        stability.record(StabilityScore::new("hal", "1.0.0"));

        let summary = store.save_all(&telemetry, &workflows, &stability).unwrap();
        assert_eq!(summary.telemetry_entries, 1);
        assert_eq!(summary.workflow_entries, 1);
        assert_eq!(summary.stability_entries, 1);

        cleanup(&path);
    }
}
