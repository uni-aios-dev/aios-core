use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEntry {
    pub metric_name: String,
    pub value: f64,
    pub ram_used_mb: u64,
    pub timestamp_ms: u64,
    pub block_id: Option<u32>,
    pub process_name: Option<String>,
}

impl TelemetryEntry {
    pub fn new(metric_name: &str, value: f64, ram_used_mb: u64) -> Self {
        Self {
            metric_name: metric_name.to_string(),
            value,
            ram_used_mb,
            timestamp_ms: now_ms(),
            block_id: None,
            process_name: None,
        }
    }

    pub fn with_block(mut self, block_id: u32) -> Self {
        self.block_id = Some(block_id);
        self
    }

    pub fn with_process(mut self, name: &str) -> Self {
        self.process_name = Some(name.to_string());
        self
    }
}

pub struct TelemetryStore {
    pub entries: Vec<TelemetryEntry>,
    max_entries: usize,
}

impl Default for TelemetryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryStore {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 10_000,
        }
    }

    pub fn record(&mut self, entry: TelemetryEntry) {
        if self.entries.len() >= self.max_entries {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    pub fn query_metric(&self, name: &str) -> Vec<&TelemetryEntry> {
        self.entries
            .iter()
            .filter(|e| e.metric_name == name)
            .collect()
    }

    pub fn query_range(&self, start_ms: u64, end_ms: u64) -> Vec<&TelemetryEntry> {
        self.entries
            .iter()
            .filter(|e| e.timestamp_ms >= start_ms && e.timestamp_ms <= end_ms)
            .collect()
    }

    pub fn query_by_block(&self, block_id: u32) -> Vec<&TelemetryEntry> {
        self.entries
            .iter()
            .filter(|e| e.block_id == Some(block_id))
            .collect()
    }

    pub fn latest(&self) -> Option<&TelemetryEntry> {
        self.entries.last()
    }

    pub fn average_value(&self, name: &str) -> Option<f64> {
        let values: Vec<f64> = self
            .entries
            .iter()
            .filter(|e| e.metric_name == name)
            .map(|e| e.value)
            .collect();
        if values.is_empty() {
            None
        } else {
            Some(values.iter().sum::<f64>() / values.len() as f64)
        }
    }

    pub fn peak_ram(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| e.ram_used_mb)
            .max()
            .unwrap_or(0)
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
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
    fn test_record_and_query() {
        let mut store = TelemetryStore::new();
        store.record(TelemetryEntry::new("cpu", 75.0, 1024));
        store.record(TelemetryEntry::new("cpu", 80.0, 2048));
        assert_eq!(store.count(), 2);
        assert_eq!(store.query_metric("cpu").len(), 2);
    }

    #[test]
    fn test_average_value() {
        let mut store = TelemetryStore::new();
        store.record(TelemetryEntry::new("cpu", 10.0, 100));
        store.record(TelemetryEntry::new("cpu", 20.0, 100));
        store.record(TelemetryEntry::new("gpu", 50.0, 100));
        assert_eq!(store.average_value("cpu"), Some(15.0));
        assert_eq!(store.average_value("gpu"), Some(50.0));
        assert_eq!(store.average_value("net"), None);
    }

    #[test]
    fn test_peak_ram() {
        let mut store = TelemetryStore::new();
        store.record(TelemetryEntry::new("cpu", 50.0, 1000));
        store.record(TelemetryEntry::new("cpu", 50.0, 4000));
        assert_eq!(store.peak_ram(), 4000);
    }

    #[test]
    fn test_max_entries_fifo() {
        let mut store = TelemetryStore::new();
        store.max_entries = 3;
        store.record(TelemetryEntry::new("a", 1.0, 100));
        store.record(TelemetryEntry::new("b", 2.0, 100));
        store.record(TelemetryEntry::new("c", 3.0, 100));
        store.record(TelemetryEntry::new("d", 4.0, 100));
        assert_eq!(store.count(), 3);
        assert_eq!(store.entries[0].metric_name, "b");
    }

    #[test]
    fn test_query_by_block() {
        let mut store = TelemetryStore::new();
        store.record(TelemetryEntry::new("cpu", 50.0, 100).with_block(1));
        store.record(TelemetryEntry::new("cpu", 60.0, 100).with_block(2));
        assert_eq!(store.query_by_block(1).len(), 1);
    }
}
