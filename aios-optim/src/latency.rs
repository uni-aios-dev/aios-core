use std::collections::HashMap;
use std::time::Instant;

pub struct LatencyTracker {
    buckets: HashMap<String, Vec<u64>>,
    thresholds: HashMap<String, LatencyThreshold>,
    max_samples_per_op: usize,
}

#[derive(Debug, Clone)]
pub struct LatencyThreshold {
    pub warn_us: u64,
    pub critical_us: u64,
}

#[derive(Debug, Clone)]
pub struct LatencyStats {
    pub operation: String,
    pub count: usize,
    pub min_us: u64,
    pub max_us: u64,
    pub avg_us: f64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub violations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatencyLevel {
    Normal,
    Warning,
    Critical,
}

pub struct LatencyGuard<'a> {
    tracker: &'a mut LatencyTracker,
    operation: String,
    start: Instant,
}

impl LatencyTracker {
    pub fn new(max_samples_per_op: usize) -> Self {
        Self {
            buckets: HashMap::new(),
            thresholds: HashMap::new(),
            max_samples_per_op,
        }
    }

    pub fn with_threshold(mut self, operation: &str, threshold: LatencyThreshold) -> Self {
        self.thresholds.insert(operation.to_string(), threshold);
        self
    }

    pub fn record(&mut self, operation: &str, duration_us: u64) -> LatencyLevel {
        let bucket = self.buckets.entry(operation.to_string()).or_default();
        if bucket.len() >= self.max_samples_per_op {
            bucket.remove(0);
        }
        bucket.push(duration_us);

        self.classify(operation, duration_us)
    }

    pub fn guard(&mut self, operation: &str) -> LatencyGuard<'_> {
        LatencyGuard {
            tracker: self,
            operation: operation.to_string(),
            start: Instant::now(),
        }
    }

    pub fn classify(&self, operation: &str, duration_us: u64) -> LatencyLevel {
        if let Some(threshold) = self.thresholds.get(operation) {
            if duration_us >= threshold.critical_us {
                LatencyLevel::Critical
            } else if duration_us >= threshold.warn_us {
                LatencyLevel::Warning
            } else {
                LatencyLevel::Normal
            }
        } else {
            LatencyLevel::Normal
        }
    }

    pub fn stats(&self, operation: &str) -> Option<LatencyStats> {
        let values = self.buckets.get(operation)?;
        if values.is_empty() {
            return None;
        }

        let mut sorted = values.clone();
        sorted.sort_unstable();

        let min_us = sorted[0];
        let max_us = sorted[sorted.len() - 1];
        let sum: u64 = sorted.iter().sum();
        let avg_us = sum as f64 / sorted.len() as f64;

        let p50_us = percentile(&sorted, 0.50);
        let p95_us = percentile(&sorted, 0.95);
        let p99_us = percentile(&sorted, 0.99);

        let violations = if let Some(threshold) = self.thresholds.get(operation) {
            sorted.iter().filter(|&&v| v >= threshold.warn_us).count() as u64
        } else {
            0
        };

        Some(LatencyStats {
            operation: operation.to_string(),
            count: sorted.len(),
            min_us,
            max_us,
            avg_us,
            p50_us,
            p95_us,
            p99_us,
            violations,
        })
    }

    pub fn all_stats(&self) -> Vec<LatencyStats> {
        let mut ops: Vec<String> = self.buckets.keys().cloned().collect();
        ops.sort();
        ops.iter().filter_map(|op| self.stats(op)).collect()
    }

    pub fn operations(&self) -> Vec<String> {
        let mut ops: Vec<String> = self.buckets.keys().cloned().collect();
        ops.sort();
        ops
    }

    pub fn count(&self, operation: &str) -> usize {
        self.buckets.get(operation).map_or(0, |v| v.len())
    }

    pub fn total_samples(&self) -> usize {
        self.buckets.values().map(|v| v.len()).sum()
    }

    pub fn clear(&mut self) {
        self.buckets.clear();
    }

    pub fn clear_operation(&mut self, operation: &str) {
        self.buckets.remove(operation);
    }

    pub fn violations(&self, operation: &str) -> u64 {
        self.stats(operation).map_or(0, |s| s.violations)
    }

    pub fn total_violations(&self) -> u64 {
        self.buckets.keys().map(|op| self.violations(op)).sum()
    }
}

impl<'a> LatencyGuard<'a> {
    pub fn stop(self) -> LatencyLevel {
        let us = self.start.elapsed().as_micros() as u64;
        self.tracker.record(&self.operation, us)
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p) as usize;
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_classify() {
        let mut tracker = LatencyTracker::new(1000).with_threshold(
            "slow_op",
            LatencyThreshold {
                warn_us: 100,
                critical_us: 500,
            },
        );
        assert_eq!(tracker.record("fast_op", 5), LatencyLevel::Normal);
        assert_eq!(tracker.record("slow_op", 50), LatencyLevel::Normal);
        assert_eq!(tracker.record("slow_op", 150), LatencyLevel::Warning);
        assert_eq!(tracker.record("slow_op", 600), LatencyLevel::Critical);
    }

    #[test]
    fn test_stats() {
        let mut tracker = LatencyTracker::new(1000);
        for i in 0..100 {
            tracker.record("op", i);
        }
        let stats = tracker.stats("op").unwrap();
        assert_eq!(stats.count, 100);
        assert_eq!(stats.min_us, 0);
        assert_eq!(stats.max_us, 99);
        assert!((stats.avg_us - 49.5).abs() < 0.1);
        assert_eq!(stats.p50_us, 49);
        assert_eq!(stats.p95_us, 94);
        assert_eq!(stats.p99_us, 98);
    }

    #[test]
    fn test_guard() {
        let mut tracker = LatencyTracker::new(1000);
        let level = {
            let guard = tracker.guard("timed_op");
            std::thread::sleep(std::time::Duration::from_millis(1));
            guard.stop()
        };
        assert_eq!(level, LatencyLevel::Normal);
        assert_eq!(tracker.count("timed_op"), 1);
    }

    #[test]
    fn test_violations() {
        let mut tracker = LatencyTracker::new(1000).with_threshold(
            "op",
            LatencyThreshold {
                warn_us: 100,
                critical_us: 500,
            },
        );
        tracker.record("op", 50);
        tracker.record("op", 150);
        tracker.record("op", 600);
        assert_eq!(tracker.violations("op"), 2);
        assert_eq!(tracker.total_violations(), 2);
    }

    #[test]
    fn test_all_stats_sorted() {
        let mut tracker = LatencyTracker::new(100);
        tracker.record("a", 1);
        tracker.record("b", 2);
        tracker.record("c", 3);
        let all = tracker.all_stats();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].operation, "a");
        assert_eq!(all[1].operation, "b");
        assert_eq!(all[2].operation, "c");
    }

    #[test]
    fn test_fifo_eviction() {
        let mut tracker = LatencyTracker::new(5);
        for i in 0..10 {
            tracker.record("op", i);
        }
        assert_eq!(tracker.count("op"), 5);
        let stats = tracker.stats("op").unwrap();
        assert_eq!(stats.min_us, 5);
        assert_eq!(stats.max_us, 9);
    }

    #[test]
    fn test_clear() {
        let mut tracker = LatencyTracker::new(100);
        tracker.record("a", 1);
        tracker.record("b", 2);
        assert_eq!(tracker.total_samples(), 2);
        tracker.clear();
        assert_eq!(tracker.total_samples(), 0);
    }

    #[test]
    fn test_clear_operation() {
        let mut tracker = LatencyTracker::new(100);
        tracker.record("a", 1);
        tracker.record("b", 2);
        tracker.clear_operation("a");
        assert_eq!(tracker.count("a"), 0);
        assert_eq!(tracker.count("b"), 1);
    }

    #[test]
    fn test_empty_stats() {
        let tracker = LatencyTracker::new(100);
        assert!(tracker.stats("nonexistent").is_none());
    }

    #[test]
    fn test_operations() {
        let mut tracker = LatencyTracker::new(100);
        tracker.record("x", 1);
        tracker.record("a", 2);
        tracker.record("m", 3);
        let ops = tracker.operations();
        assert_eq!(ops, vec!["a", "m", "x"]);
    }
}
