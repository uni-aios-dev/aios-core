use std::time::Instant;

#[derive(Debug, Clone)]
pub struct LatencySample {
    pub operation: String,
    pub duration_us: u64,
    pub timestamp_ms: u64,
}

pub struct LatencyProfiler {
    samples: Vec<LatencySample>,
    max_samples: usize,
}

impl LatencyProfiler {
    pub fn new(max_samples: usize) -> Self {
        Self {
            samples: Vec::with_capacity(max_samples.min(10_000)),
            max_samples,
        }
    }

    pub fn record(&mut self, operation: &str, duration_us: u64) {
        if self.samples.len() >= self.max_samples {
            self.samples.remove(0);
        }
        self.samples.push(LatencySample {
            operation: operation.to_string(),
            duration_us,
            timestamp_ms: now_ms(),
        });
    }

    pub fn start(&self) -> Instant {
        Instant::now()
    }

    pub fn stop(&mut self, operation: &str, start: Instant) {
        let us = start.elapsed().as_micros() as u64;
        self.record(operation, us);
    }

    pub fn p50(&self, operation: &str) -> Option<u64> {
        self.percentile(operation, 0.50)
    }

    pub fn p95(&self, operation: &str) -> Option<u64> {
        self.percentile(operation, 0.95)
    }

    pub fn p99(&self, operation: &str) -> Option<u64> {
        self.percentile(operation, 0.99)
    }

    pub fn avg(&self, operation: &str) -> Option<f64> {
        let values: Vec<u64> = self
            .samples
            .iter()
            .filter(|s| s.operation == operation)
            .map(|s| s.duration_us)
            .collect();
        if values.is_empty() {
            None
        } else {
            let sum: u64 = values.iter().sum();
            Some(sum as f64 / values.len() as f64)
        }
    }

    pub fn count(&self, operation: &str) -> usize {
        self.samples
            .iter()
            .filter(|s| s.operation == operation)
            .count()
    }

    pub fn total_samples(&self) -> usize {
        self.samples.len()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn operations(&self) -> Vec<String> {
        let mut ops: Vec<String> = self
            .samples
            .iter()
            .map(|s| s.operation.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        ops.sort();
        ops
    }

    fn percentile(&self, operation: &str, p: f64) -> Option<u64> {
        let mut values: Vec<u64> = self
            .samples
            .iter()
            .filter(|s| s.operation == operation)
            .map(|s| s.duration_us)
            .collect();
        if values.is_empty() {
            return None;
        }
        values.sort_unstable();
        let idx = ((values.len() as f64 - 1.0) * p) as usize;
        Some(values[idx])
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
    fn test_record_and_avg() {
        let mut p = LatencyProfiler::new(1000);
        p.record("ipc_send", 10);
        p.record("ipc_send", 20);
        p.record("ipc_send", 30);
        assert_eq!(p.count("ipc_send"), 3);
        assert_eq!(p.avg("ipc_send"), Some(20.0));
    }

    #[test]
    fn test_percentiles() {
        let mut p = LatencyProfiler::new(1000);
        for i in 0..100 {
            p.record("op", i);
        }
        assert_eq!(p.p50("op"), Some(49));
        assert_eq!(p.p99("op"), Some(98));
    }

    #[test]
    fn test_fifo_eviction() {
        let mut p = LatencyProfiler::new(5);
        for i in 0..10 {
            p.record("op", i);
        }
        assert_eq!(p.total_samples(), 5);
    }

    #[test]
    fn test_operations_list() {
        let mut p = LatencyProfiler::new(100);
        p.record("a", 1);
        p.record("b", 2);
        p.record("a", 3);
        let mut ops = p.operations();
        ops.sort();
        assert_eq!(ops, vec!["a", "b"]);
    }

    #[test]
    fn test_clear() {
        let mut p = LatencyProfiler::new(100);
        p.record("op", 5);
        assert_eq!(p.total_samples(), 1);
        p.clear();
        assert_eq!(p.total_samples(), 0);
    }

    #[test]
    fn test_empty_percentile() {
        let p = LatencyProfiler::new(100);
        assert_eq!(p.p50("nonexistent"), None);
        assert_eq!(p.avg("nonexistent"), None);
    }

    #[test]
    fn test_start_stop() {
        let mut p = LatencyProfiler::new(100);
        let start = p.start();
        std::thread::sleep(std::time::Duration::from_millis(1));
        p.stop("test_op", start);
        assert_eq!(p.count("test_op"), 1);
        assert!(p.avg("test_op").unwrap() > 0.0);
    }
}
