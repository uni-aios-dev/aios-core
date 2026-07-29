use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct MetricCollector {
    counters: HashMap<String, u64>,
    gauges: HashMap<String, f64>,
    histograms: HashMap<String, Vec<f64>>,
    label_prefix: String,
}

impl MetricCollector {
    pub fn new(label_prefix: &str) -> Self {
        Self {
            counters: HashMap::new(),
            gauges: HashMap::new(),
            histograms: HashMap::new(),
            label_prefix: label_prefix.to_string(),
        }
    }

    pub fn increment_counter(&mut self, name: &str, value: u64) {
        *self.counters.entry(name.to_string()).or_insert(0) += value;
    }

    pub fn set_gauge(&mut self, name: &str, value: f64) {
        self.gauges.insert(name.to_string(), value);
    }

    pub fn observe_histogram(&mut self, name: &str, value: f64) {
        self.histograms
            .entry(name.to_string())
            .or_default()
            .push(value);
    }

    pub fn get_counter(&self, name: &str) -> u64 {
        self.counters.get(name).copied().unwrap_or(0)
    }

    pub fn get_gauge(&self, name: &str) -> f64 {
        self.gauges.get(name).copied().unwrap_or(0.0)
    }

    pub fn snapshot(&self) -> MetricSnapshot {
        MetricSnapshot {
            timestamp_ms: now_ms(),
            counters: self.counters.clone(),
            gauges: self.gauges.clone(),
            histogram_stats: self.compute_histogram_stats(),
        }
    }

    pub fn to_prometheus(&self) -> String {
        let mut output = String::new();
        let prefix = &self.label_prefix;

        for (name, value) in &self.counters {
            output.push_str(&format!("# HELP {prefix}_{name} Counter\n"));
            output.push_str(&format!("# TYPE {prefix}_{name} counter\n"));
            output.push_str(&format!("{prefix}_{name} {value}\n"));
        }

        for (name, value) in &self.gauges {
            output.push_str(&format!("# HELP {prefix}_{name} Gauge\n"));
            output.push_str(&format!("# TYPE {prefix}_{name} gauge\n"));
            output.push_str(&format!("{prefix}_{name} {value}\n"));
        }

        for (name, stats) in self.compute_histogram_stats() {
            output.push_str(&format!("# HELP {prefix}_{name} Histogram\n"));
            output.push_str(&format!("# TYPE {prefix}_{name} histogram\n"));
            output.push_str(&format!(
                "{prefix}_{name}_count {}\n", stats.count
            ));
            output.push_str(&format!(
                "{prefix}_{name}_sum {}\n", stats.sum
            ));
            output.push_str(&format!(
                "{prefix}_{name}_min {}\n", stats.min
            ));
            output.push_str(&format!(
                "{prefix}_{name}_max {}\n", stats.max
            ));
            output.push_str(&format!(
                "{prefix}_{name}_avg {}\n", stats.avg
            ));
        }

        output
    }

    fn compute_histogram_stats(&self) -> HashMap<String, HistogramStats> {
        let mut stats = HashMap::new();
        for (name, values) in &self.histograms {
            if values.is_empty() {
                continue;
            }
            let count = values.len() as u64;
            let sum: f64 = values.iter().sum();
            let min = values.iter().cloned().fold(f64::MAX, f64::min);
            let max = values.iter().cloned().fold(f64::MIN, f64::max);
            let avg = sum / count as f64;

            stats.insert(
                name.clone(),
                HistogramStats {
                    count,
                    sum,
                    min,
                    max,
                    avg,
                },
            );
        }
        stats
    }
}

#[derive(Debug, Clone)]
pub struct MetricSnapshot {
    pub timestamp_ms: u128,
    pub counters: HashMap<String, u64>,
    pub gauges: HashMap<String, f64>,
    pub histogram_stats: HashMap<String, HistogramStats>,
}

#[derive(Debug, Clone)]
pub struct HistogramStats {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter() {
        let mut mc = MetricCollector::new("aios");
        mc.increment_counter("requests_total", 1);
        mc.increment_counter("requests_total", 2);
        assert_eq!(mc.get_counter("requests_total"), 3);
    }

    #[test]
    fn test_gauge() {
        let mut mc = MetricCollector::new("aios");
        mc.set_gauge("ram_used_mb", 1024.0);
        assert!((mc.get_gauge("ram_used_mb") - 1024.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_histogram() {
        let mut mc = MetricCollector::new("aios");
        mc.observe_histogram("latency_ms", 10.0);
        mc.observe_histogram("latency_ms", 20.0);
        mc.observe_histogram("latency_ms", 30.0);
        let snapshot = mc.snapshot();
        let stats = &snapshot.histogram_stats["latency_ms"];
        assert_eq!(stats.count, 3);
        assert!((stats.avg - 20.0).abs() < 0.001);
        assert!((stats.min - 10.0).abs() < 0.001);
        assert!((stats.max - 30.0).abs() < 0.001);
    }

    #[test]
    fn test_prometheus_output() {
        let mut mc = MetricCollector::new("aios");
        mc.increment_counter("test_counter", 42);
        mc.set_gauge("test_gauge", 3.14);
        let output = mc.to_prometheus();
        assert!(output.contains("aios_test_counter"));
        assert!(output.contains("42"));
        assert!(output.contains("aios_test_gauge"));
        assert!(output.contains("3.14"));
    }

    #[test]
    fn test_snapshot() {
        let mut mc = MetricCollector::new("aios");
        mc.set_gauge("cpu", 0.5);
        let snap = mc.snapshot();
        assert!(snap.timestamp_ms > 0);
        assert!((snap.gauges["cpu"] - 0.5).abs() < f64::EPSILON);
    }
}
