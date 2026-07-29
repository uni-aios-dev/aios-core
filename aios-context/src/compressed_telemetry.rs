use crate::telemetry::TelemetryEntry;
use aios_compress::compressor::StateCompressor;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct CompressedTelemetryStore {
    compressor: StateCompressor,
    hot_entries: Vec<TelemetryEntry>,
    compressed_cold: Mutex<HashMap<String, Vec<u8>>>,
    hot_threshold: usize,
}

impl CompressedTelemetryStore {
    pub fn new(compression_level: i32, hot_threshold: usize) -> Self {
        Self {
            compressor: StateCompressor::with_level(compression_level)
                .unwrap_or_else(|_| StateCompressor::new()),
            hot_entries: Vec::new(),
            compressed_cold: Mutex::new(HashMap::new()),
            hot_threshold,
        }
    }

    pub fn record(&mut self, entry: TelemetryEntry) {
        self.hot_entries.push(entry);
        if self.hot_entries.len() > self.hot_threshold {
            self.compress_old_entries();
        }
    }

    fn compress_old_entries(&mut self) {
        if self.hot_entries.len() <= self.hot_threshold / 2 {
            return;
        }
        let split = self.hot_entries.len() / 2;
        let cold_entries: Vec<TelemetryEntry> = self.hot_entries.drain(..split).collect();

        if let Ok(serialized) = bincode::serialize(&cold_entries) {
            if let Ok(compressed) = self.compressor.compress(&serialized) {
                let key = format!("chunk_{}", chrono_block_name(&cold_entries));
                let mut cold = self.compressed_cold.lock().unwrap();
                cold.insert(key, compressed);
            }
        }
    }

    pub fn query_metric(&self, name: &str) -> Vec<&TelemetryEntry> {
        self.hot_entries
            .iter()
            .filter(|e| e.metric_name == name)
            .collect()
    }

    pub fn total_entries(&self) -> usize {
        let cold_count: usize = {
            let cold = self.compressed_cold.lock().unwrap();
            cold.values()
                .map(|v| {
                    self.compressor
                        .decompress(v)
                        .ok()
                        .and_then(|d| bincode::deserialize::<Vec<TelemetryEntry>>(&d).ok())
                        .map(|e| e.len())
                        .unwrap_or(0)
                })
                .sum()
        };
        self.hot_entries.len() + cold_count
    }

    pub fn hot_entries(&self) -> &[TelemetryEntry] {
        &self.hot_entries
    }

    pub fn compressed_chunks(&self) -> usize {
        self.compressed_cold.lock().unwrap().len()
    }

    pub fn compression_ratio(&self) -> f32 {
        let cold = self.compressed_cold.lock().unwrap();
        if cold.is_empty() {
            return 1.0;
        }
        let total_compressed: usize = cold.values().map(|v| v.len()).sum();
        let total_decompressed: usize = cold
            .values()
            .filter_map(|v| self.compressor.decompress(v).ok())
            .filter_map(|d| bincode::deserialize::<Vec<TelemetryEntry>>(&d).ok())
            .map(|e| bincode::serialize(&e).map(|b| b.len()).unwrap_or(0))
            .sum();
        if total_compressed == 0 {
            1.0
        } else {
            total_decompressed as f32 / total_compressed as f32
        }
    }

    pub fn clear(&mut self) {
        self.hot_entries.clear();
        self.compressed_cold.lock().unwrap().clear();
    }
}

fn chrono_block_name(entries: &[TelemetryEntry]) -> String {
    entries
        .first()
        .map(|e| format!("{}_{}", e.metric_name, e.timestamp_ms))
        .unwrap_or_else(|| "empty".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(name: &str, value: f64) -> TelemetryEntry {
        TelemetryEntry::new(name, value, 1024)
    }

    #[test]
    fn test_record_hot() {
        let mut store = CompressedTelemetryStore::new(3, 100);
        store.record(make_entry("cpu", 50.0));
        assert_eq!(store.hot_entries().len(), 1);
        assert_eq!(store.total_entries(), 1);
    }

    #[test]
    fn test_auto_compression() {
        let mut store = CompressedTelemetryStore::new(3, 10);
        for i in 0..20 {
            store.record(make_entry("cpu", i as f64));
        }
        assert!(store.compressed_chunks() > 0);
        assert!(store.hot_entries().len() <= 10);
    }

    #[test]
    fn test_query_metric() {
        let mut store = CompressedTelemetryStore::new(3, 100);
        store.record(make_entry("cpu", 50.0));
        store.record(make_entry("gpu", 80.0));
        store.record(make_entry("cpu", 60.0));
        let cpu_entries = store.query_metric("cpu");
        assert_eq!(cpu_entries.len(), 2);
    }

    #[test]
    fn test_compression_ratio() {
        let mut store = CompressedTelemetryStore::new(3, 5);
        for i in 0..20 {
            store.record(make_entry("cpu", i as f64));
        }
        let ratio = store.compression_ratio();
        assert!(
            ratio >= 1.0,
            "Compression ratio should be >= 1.0, got {}",
            ratio
        );
    }

    #[test]
    fn test_clear() {
        let mut store = CompressedTelemetryStore::new(3, 10);
        for i in 0..20 {
            store.record(make_entry("cpu", i as f64));
        }
        store.clear();
        assert_eq!(store.hot_entries().len(), 0);
        assert_eq!(store.compressed_chunks(), 0);
        assert_eq!(store.total_entries(), 0);
    }

    #[test]
    fn test_no_compression_below_threshold() {
        let mut store = CompressedTelemetryStore::new(3, 100);
        for i in 0..10 {
            store.record(make_entry("cpu", i as f64));
        }
        assert_eq!(store.compressed_chunks(), 0);
        assert_eq!(store.hot_entries().len(), 10);
    }
}
