use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub struct HotPathCounter {
    pub name: String,
    pub total_count: AtomicU64,
    pub total_duration_us: AtomicU64,
    pub max_duration_us: AtomicU64,
}

pub struct HotPath {
    counters: Mutex<HashMap<String, HotPathCounter>>,
    enabled: bool,
}

impl HotPath {
    pub fn new() -> Self {
        Self {
            counters: Mutex::new(HashMap::new()),
            enabled: true,
        }
    }

    pub fn with_enabled(enabled: bool) -> Self {
        Self {
            counters: Mutex::new(HashMap::new()),
            enabled,
        }
    }

    pub fn enter(&self, name: &str) -> HotPathGuard<'_> {
        HotPathGuard {
            path: self,
            name: name.to_string(),
            start: if self.enabled {
                Some(Instant::now())
            } else {
                None
            },
        }
    }

    pub fn record_elapsed(&self, name: &str, duration_us: u64) {
        if !self.enabled {
            return;
        }
        let mut counters = self.counters.lock().unwrap();
        let counter = counters
            .entry(name.to_string())
            .or_insert_with(|| HotPathCounter {
                name: name.to_string(),
                total_count: AtomicU64::new(0),
                total_duration_us: AtomicU64::new(0),
                max_duration_us: AtomicU64::new(0),
            });
        counter.total_count.fetch_add(1, Ordering::Relaxed);
        counter
            .total_duration_us
            .fetch_add(duration_us, Ordering::Relaxed);
        counter
            .max_duration_us
            .fetch_max(duration_us, Ordering::Relaxed);
    }

    pub fn stats(&self) -> Vec<(String, u64, f64, u64)> {
        let counters = self.counters.lock().unwrap();
        counters
            .values()
            .map(|c| {
                let count = c.total_count.load(Ordering::Relaxed);
                let total = c.total_duration_us.load(Ordering::Relaxed);
                let max = c.max_duration_us.load(Ordering::Relaxed);
                let avg = if count > 0 {
                    total as f64 / count as f64
                } else {
                    0.0
                };
                (c.name.clone(), count, avg, max)
            })
            .collect()
    }

    pub fn count(&self, name: &str) -> u64 {
        let counters = self.counters.lock().unwrap();
        counters
            .get(name)
            .map(|c| c.total_count.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn avg_us(&self, name: &str) -> f64 {
        let counters = self.counters.lock().unwrap();
        if let Some(c) = counters.get(name) {
            let count = c.total_count.load(Ordering::Relaxed);
            let total = c.total_duration_us.load(Ordering::Relaxed);
            if count > 0 {
                total as f64 / count as f64
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    pub fn clear(&self) {
        self.counters.lock().unwrap().clear();
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

pub struct HotPathGuard<'a> {
    path: &'a HotPath,
    name: String,
    start: Option<Instant>,
}

impl<'a> Drop for HotPathGuard<'a> {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            let us = start.elapsed().as_micros() as u64;
            self.path.record_elapsed(&self.name, us);
        }
    }
}

impl Default for HotPath {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hotpath_record() {
        let hp = HotPath::new();
        hp.record_elapsed("ipc_send", 5);
        hp.record_elapsed("ipc_send", 15);
        assert_eq!(hp.count("ipc_send"), 2);
        assert_eq!(hp.avg_us("ipc_send"), 10.0);
    }

    #[test]
    fn test_hotpath_guard() {
        let hp = HotPath::new();
        {
            let _guard = hp.enter("sched_dispatch");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(hp.count("sched_dispatch"), 1);
        assert!(hp.avg_us("sched_dispatch") > 0.0);
    }

    #[test]
    fn test_hotpath_max_tracking() {
        let hp = HotPath::new();
        hp.record_elapsed("op", 5);
        hp.record_elapsed("op", 20);
        hp.record_elapsed("op", 10);
        let stats = hp.stats();
        let (_, count, _, max) = stats.iter().find(|(n, _, _, _)| n == "op").unwrap();
        assert_eq!(*count, 3);
        assert_eq!(*max, 20);
    }

    #[test]
    fn test_hotpath_disabled() {
        let hp = HotPath::with_enabled(false);
        {
            let _guard = hp.enter("op");
        }
        assert_eq!(hp.count("op"), 0);
    }

    #[test]
    fn test_hotpath_stats() {
        let hp = HotPath::new();
        hp.record_elapsed("a", 10);
        hp.record_elapsed("b", 20);
        let stats = hp.stats();
        assert_eq!(stats.len(), 2);
    }

    #[test]
    fn test_hotpath_clear() {
        let hp = HotPath::new();
        hp.record_elapsed("op", 5);
        hp.clear();
        assert_eq!(hp.count("op"), 0);
    }
}
