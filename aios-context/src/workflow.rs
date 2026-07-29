use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowProfile {
    pub name: String,
    pub trigger_blocks: Vec<String>,
    pub recommended_priorities: HashMap<String, u8>,
    pub usage_count: u64,
    pub last_used_ms: u64,
}

impl WorkflowProfile {
    pub fn new(name: &str, trigger_blocks: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            trigger_blocks,
            recommended_priorities: HashMap::new(),
            usage_count: 0,
            last_used_ms: now_ms(),
        }
    }

    pub fn set_priority(&mut self, process_name: &str, priority: u8) {
        self.recommended_priorities
            .insert(process_name.to_string(), priority);
    }

    pub fn get_priority(&self, process_name: &str) -> Option<u8> {
        self.recommended_priorities.get(process_name).copied()
    }
}

pub struct WorkflowStore {
    pub profiles: HashMap<String, WorkflowProfile>,
}

impl Default for WorkflowStore {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowStore {
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
        }
    }

    pub fn record(&mut self, name: String, trigger_blocks: Vec<String>) {
        let entry = self
            .profiles
            .entry(name.clone())
            .or_insert_with(|| WorkflowProfile::new(&name, trigger_blocks));
        entry.usage_count += 1;
        entry.last_used_ms = now_ms();
    }

    pub fn get(&self, name: &str) -> Option<&WorkflowProfile> {
        self.profiles.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut WorkflowProfile> {
        self.profiles.get_mut(name)
    }

    pub fn most_used(&self) -> Option<&WorkflowProfile> {
        self.profiles.values().max_by_key(|p| p.usage_count)
    }

    pub fn recently_used(&self, within_ms: u64) -> Vec<&WorkflowProfile> {
        let cutoff = now_ms().saturating_sub(within_ms);
        self.profiles
            .values()
            .filter(|p| p.last_used_ms >= cutoff)
            .collect()
    }

    pub fn count(&self) -> usize {
        self.profiles.len()
    }

    pub fn clear(&mut self) {
        self.profiles.clear();
    }
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

    #[test]
    fn test_record_workflow() {
        let mut store = WorkflowStore::new();
        store.record("video_editing".into(), vec!["render_block".into()]);
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn test_record_increments_usage() {
        let mut store = WorkflowStore::new();
        store.record("editing".into(), vec![]);
        store.record("editing".into(), vec![]);
        let profile = store.get("editing").unwrap();
        assert_eq!(profile.usage_count, 2);
    }

    #[test]
    fn test_most_used() {
        let mut store = WorkflowStore::new();
        store.record("a".into(), vec![]);
        store.record("a".into(), vec![]);
        store.record("b".into(), vec![]);
        assert_eq!(store.most_used().unwrap().name, "a");
    }

    #[test]
    fn test_set_priority() {
        let mut store = WorkflowStore::new();
        store.record("coding".into(), vec![]);
        store
            .get_mut("coding")
            .unwrap()
            .set_priority("ai_orchestrator", 4);
        assert_eq!(
            store.get("coding").unwrap().get_priority("ai_orchestrator"),
            Some(4)
        );
    }

    #[test]
    fn test_clear() {
        let mut store = WorkflowStore::new();
        store.record("x".into(), vec![]);
        store.clear();
        assert_eq!(store.count(), 0);
    }
}
