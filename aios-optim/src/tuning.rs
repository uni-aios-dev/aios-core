use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningConfig {
    pub time_slice_ms: u64,
    pub aging_threshold_ms: u64,
    pub max_queue_size: usize,
    pub memory_pressure_threshold: f32,
    pub heartbeat_interval_ms: u64,
}

impl Default for TuningConfig {
    fn default() -> Self {
        Self {
            time_slice_ms: 10,
            aging_threshold_ms: 500,
            max_queue_size: 1024,
            memory_pressure_threshold: 0.8,
            heartbeat_interval_ms: 1000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThroughputMetrics {
    pub ipc_throughput_per_sec: f64,
    pub avg_schedule_latency_us: f64,
    pub ram_usage_ratio: f32,
    pub block_count: usize,
    pub process_count: usize,
}

pub struct AutoTuner {
    config: TuningConfig,
    baseline: TuningConfig,
    adjustments: Vec<TuningAdjustment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningAdjustment {
    pub field: String,
    pub old_value: String,
    pub new_value: String,
    pub reason: String,
    pub timestamp_ms: u64,
}

impl AutoTuner {
    pub fn new(config: TuningConfig) -> Self {
        Self {
            baseline: config.clone(),
            config,
            adjustments: Vec::new(),
        }
    }

    pub fn config(&self) -> &TuningConfig {
        &self.config
    }

    pub fn analyze_and_tune(&mut self, metrics: &ThroughputMetrics) {
        self.tune_time_slice(metrics);
        self.tune_aging_threshold(metrics);
        self.tune_queue_size(metrics);
    }

    fn tune_time_slice(&mut self, metrics: &ThroughputMetrics) {
        if metrics.ipc_throughput_per_sec > 10_000.0 && self.config.time_slice_ms < 50 {
            let old = self.config.time_slice_ms;
            self.config.time_slice_ms = (self.config.time_slice_ms * 2).min(50);
            self.record_adjustment(
                "time_slice_ms",
                old.to_string(),
                self.config.time_slice_ms.to_string(),
                "High IPC throughput detected, increasing time slice".into(),
            );
        } else if metrics.avg_schedule_latency_us > 100.0 && self.config.time_slice_ms > 1 {
            let old = self.config.time_slice_ms;
            self.config.time_slice_ms = (self.config.time_slice_ms / 2).max(1);
            self.record_adjustment(
                "time_slice_ms",
                old.to_string(),
                self.config.time_slice_ms.to_string(),
                "High scheduling latency detected, decreasing time slice".into(),
            );
        }
    }

    fn tune_aging_threshold(&mut self, metrics: &ThroughputMetrics) {
        if metrics.process_count > 100 && self.config.aging_threshold_ms < 5000 {
            let old = self.config.aging_threshold_ms;
            self.config.aging_threshold_ms = (self.config.aging_threshold_ms * 2).min(5000);
            self.record_adjustment(
                "aging_threshold_ms",
                old.to_string(),
                self.config.aging_threshold_ms.to_string(),
                "High process count, increasing aging threshold to reduce overhead".into(),
            );
        }
    }

    fn tune_queue_size(&mut self, metrics: &ThroughputMetrics) {
        if metrics.ipc_throughput_per_sec > 50_000.0 && self.config.max_queue_size < 65536 {
            let old = self.config.max_queue_size;
            self.config.max_queue_size = (self.config.max_queue_size * 2).min(65536);
            self.record_adjustment(
                "max_queue_size",
                old.to_string(),
                self.config.max_queue_size.to_string(),
                "Very high IPC throughput, doubling queue capacity".into(),
            );
        }
    }

    fn record_adjustment(&mut self, field: &str, old: String, new: String, reason: String) {
        self.adjustments.push(TuningAdjustment {
            field: field.to_string(),
            old_value: old,
            new_value: new,
            reason,
            timestamp_ms: now_ms(),
        });
    }

    pub fn reset(&mut self) {
        self.config = self.baseline.clone();
        self.adjustments.clear();
    }

    pub fn adjustments(&self) -> &[TuningAdjustment] {
        &self.adjustments
    }

    pub fn adjustment_count(&self) -> usize {
        self.adjustments.len()
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
    fn test_default_config() {
        let config = TuningConfig::default();
        assert_eq!(config.time_slice_ms, 10);
        assert_eq!(config.aging_threshold_ms, 500);
        assert_eq!(config.max_queue_size, 1024);
    }

    #[test]
    fn test_high_throughput_increases_time_slice() {
        let mut tuner = AutoTuner::new(TuningConfig::default());
        let metrics = ThroughputMetrics {
            ipc_throughput_per_sec: 15_000.0,
            avg_schedule_latency_us: 10.0,
            ram_usage_ratio: 0.3,
            block_count: 5,
            process_count: 10,
        };
        tuner.analyze_and_tune(&metrics);
        assert!(tuner.config().time_slice_ms > 10);
        assert_eq!(tuner.adjustment_count(), 1);
    }

    #[test]
    fn test_high_latency_decreases_time_slice() {
        let mut tuner = AutoTuner::new(TuningConfig::default());
        let metrics = ThroughputMetrics {
            ipc_throughput_per_sec: 100.0,
            avg_schedule_latency_us: 200.0,
            ram_usage_ratio: 0.3,
            block_count: 5,
            process_count: 10,
        };
        tuner.analyze_and_tune(&metrics);
        assert!(tuner.config().time_slice_ms < 10);
    }

    #[test]
    fn test_high_process_count_increases_aging() {
        let mut tuner = AutoTuner::new(TuningConfig::default());
        let metrics = ThroughputMetrics {
            ipc_throughput_per_sec: 1000.0,
            avg_schedule_latency_us: 10.0,
            ram_usage_ratio: 0.3,
            block_count: 5,
            process_count: 200,
        };
        tuner.analyze_and_tune(&metrics);
        assert!(tuner.config().aging_threshold_ms > 500);
    }

    #[test]
    fn test_very_high_throughput_increases_queue() {
        let mut tuner = AutoTuner::new(TuningConfig::default());
        let metrics = ThroughputMetrics {
            ipc_throughput_per_sec: 60_000.0,
            avg_schedule_latency_us: 10.0,
            ram_usage_ratio: 0.3,
            block_count: 5,
            process_count: 10,
        };
        tuner.analyze_and_tune(&metrics);
        assert!(tuner.config().max_queue_size > 1024);
    }

    #[test]
    fn test_reset_restores_baseline() {
        let mut tuner = AutoTuner::new(TuningConfig::default());
        let metrics = ThroughputMetrics {
            ipc_throughput_per_sec: 60_000.0,
            avg_schedule_latency_us: 200.0,
            ram_usage_ratio: 0.3,
            block_count: 5,
            process_count: 200,
        };
        tuner.analyze_and_tune(&metrics);
        assert!(tuner.adjustment_count() > 0);
        tuner.reset();
        assert_eq!(tuner.adjustment_count(), 0);
        assert_eq!(tuner.config().time_slice_ms, 10);
    }

    #[test]
    fn test_no_tuning_for_normal_metrics() {
        let mut tuner = AutoTuner::new(TuningConfig::default());
        let metrics = ThroughputMetrics {
            ipc_throughput_per_sec: 1000.0,
            avg_schedule_latency_us: 5.0,
            ram_usage_ratio: 0.3,
            block_count: 5,
            process_count: 10,
        };
        tuner.analyze_and_tune(&metrics);
        assert_eq!(tuner.adjustment_count(), 0);
    }
}
