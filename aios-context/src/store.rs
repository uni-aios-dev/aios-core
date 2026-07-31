use crate::stability::StabilityStore;
use crate::telemetry::TelemetryStore;
use crate::workflow::WorkflowStore;
use serde::{Deserialize, Serialize};

pub struct EmbeddedContextStore {
    telemetry: TelemetryStore,
    workflows: WorkflowStore,
    stability: StabilityStore,
    max_entries_per_collection: usize,
    compact_threshold_ratio: f64,
}

impl EmbeddedContextStore {
    pub fn new(max_entries_per_collection: usize) -> Self {
        Self {
            telemetry: TelemetryStore::new(),
            workflows: WorkflowStore::new(),
            stability: StabilityStore::new(),
            max_entries_per_collection,
            compact_threshold_ratio: 0.8,
        }
    }

    pub fn with_compact_threshold(max_entries: usize, threshold_ratio: f64) -> Self {
        Self {
            telemetry: TelemetryStore::new(),
            workflows: WorkflowStore::new(),
            stability: StabilityStore::new(),
            max_entries_per_collection: max_entries,
            compact_threshold_ratio: threshold_ratio,
        }
    }

    pub fn should_compact(&self) -> bool {
        self.telemetry.entries.len() as f64
            >= self.max_entries_per_collection as f64 * self.compact_threshold_ratio
    }

    pub fn compact(&mut self) -> CompactReport {
        let old_telemetry = self.telemetry.entries.len();
        let old_workflows = self.workflows.profiles.len();

        let cutoff = (self.max_entries_per_collection / 2).max(1);
        if self.telemetry.entries.len() > cutoff {
            let drain_count = self.telemetry.entries.len() - cutoff;
            self.telemetry.entries.drain(..drain_count);
        }

        let now = now_ms();
        let max_age_ms = 3_600_000;
        self.workflows
            .profiles
            .retain(|_, p| now.saturating_sub(p.last_used_ms) < max_age_ms);

        let new_telemetry = self.telemetry.entries.len();
        let new_workflows = self.workflows.profiles.len();

        CompactReport {
            telemetry_pruned: old_telemetry - new_telemetry,
            workflows_pruned: old_workflows - new_workflows,
            stability_pruned: 0,
        }
    }

    pub fn telemetry(&self) -> &TelemetryStore {
        &self.telemetry
    }

    pub fn telemetry_mut(&mut self) -> &mut TelemetryStore {
        &mut self.telemetry
    }

    pub fn workflows(&self) -> &WorkflowStore {
        &self.workflows
    }

    pub fn workflows_mut(&mut self) -> &mut WorkflowStore {
        &mut self.workflows
    }

    pub fn stability(&self) -> &StabilityStore {
        &self.stability
    }

    pub fn stability_mut(&mut self) -> &mut StabilityStore {
        &mut self.stability
    }

    pub fn export_all(&self) -> ContextExport {
        ContextExport {
            telemetry_entries: self.telemetry.entries.len(),
            workflow_entries: self.workflows.profiles.len(),
            stability_entries: self.stability.scores.len(),
        }
    }

    pub fn clear(&mut self) {
        self.telemetry.clear();
        self.workflows.clear();
        self.stability.clear();
    }

    pub fn total_entries(&self) -> usize {
        self.telemetry.entries.len() + self.workflows.profiles.len() + self.stability.scores.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextExport {
    pub telemetry_entries: usize,
    pub workflow_entries: usize,
    pub stability_entries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactReport {
    pub telemetry_pruned: usize,
    pub workflows_pruned: usize,
    pub stability_pruned: usize,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::TelemetryEntry;

    #[test]
    fn test_store_creation() {
        let store = EmbeddedContextStore::new(1000);
        assert_eq!(store.total_entries(), 0);
    }

    #[test]
    fn test_export() {
        let mut store = EmbeddedContextStore::new(1000);
        store
            .telemetry_mut()
            .record(TelemetryEntry::new("cpu", 50.0, 1024));
        let export = store.export_all();
        assert_eq!(export.telemetry_entries, 1);
    }

    #[test]
    fn test_clear() {
        let mut store = EmbeddedContextStore::new(1000);
        store
            .telemetry_mut()
            .record(TelemetryEntry::new("cpu", 50.0, 1024));
        store
            .workflows_mut()
            .record("editing".into(), vec!["process_a".into()]);
        store.clear();
        assert_eq!(store.total_entries(), 0);
    }

    #[test]
    fn test_should_compact_when_above_threshold() {
        let mut store = EmbeddedContextStore::with_compact_threshold(100, 0.8);
        for i in 0..80 {
            store
                .telemetry_mut()
                .record(TelemetryEntry::new("cpu", i as f64, 512));
        }
        assert!(store.should_compact());
    }

    #[test]
    fn test_should_not_compact_when_below_threshold() {
        let mut store = EmbeddedContextStore::with_compact_threshold(100, 0.8);
        for i in 0..40 {
            store
                .telemetry_mut()
                .record(TelemetryEntry::new("cpu", i as f64, 512));
        }
        assert!(!store.should_compact());
    }

    #[test]
    fn test_compact_prunes_old_telemetry() {
        let mut store = EmbeddedContextStore::with_compact_threshold(100, 0.0);
        for i in 0..80 {
            store
                .telemetry_mut()
                .record(TelemetryEntry::new("cpu", i as f64, 512));
        }
        let report = store.compact();
        assert!(report.telemetry_pruned > 0);
        assert!(store.telemetry().entries.len() <= 50);
    }

    #[test]
    fn test_compact_preserves_minimum_data() {
        let mut store = EmbeddedContextStore::with_compact_threshold(100, 0.0);
        for i in 0..5 {
            store
                .telemetry_mut()
                .record(TelemetryEntry::new("cpu", i as f64, 512));
        }
        let report = store.compact();
        assert_eq!(report.telemetry_pruned, 0);
        assert_eq!(store.telemetry().entries.len(), 5);
    }
}
