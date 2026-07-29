use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StabilityScore {
    pub block_name: String,
    pub binary_version: String,
    pub score: f64,
    pub crash_count: u32,
    pub uptime_ms: u64,
    pub last_updated_ms: u64,
}

impl StabilityScore {
    pub fn new(block_name: &str, binary_version: &str) -> Self {
        Self {
            block_name: block_name.to_string(),
            binary_version: binary_version.to_string(),
            score: 1.0,
            crash_count: 0,
            uptime_ms: 0,
            last_updated_ms: now_ms(),
        }
    }

    pub fn record_crash(&mut self) {
        self.crash_count += 1;
        self.score = (self.score - 0.1).max(0.0);
        self.last_updated_ms = now_ms();
    }

    pub fn record_uptime(&mut self, ms: u64) {
        self.uptime_ms += ms;
        self.score = (self.score + 0.01).min(1.0);
        self.last_updated_ms = now_ms();
    }

    pub fn is_healthy(&self) -> bool {
        self.score >= 0.5
    }
}

pub struct StabilityStore {
    pub scores: Vec<StabilityScore>,
}

impl Default for StabilityStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StabilityStore {
    pub fn new() -> Self {
        Self { scores: Vec::new() }
    }

    pub fn record(&mut self, score: StabilityScore) {
        if let Some(existing) = self
            .scores
            .iter_mut()
            .find(|s| s.block_name == score.block_name && s.binary_version == score.binary_version)
        {
            existing.score = score.score;
            existing.crash_count = score.crash_count;
            existing.uptime_ms = score.uptime_ms;
            existing.last_updated_ms = score.last_updated_ms;
        } else {
            self.scores.push(score);
        }
    }

    pub fn get(&self, block_name: &str, version: &str) -> Option<&StabilityScore> {
        self.scores
            .iter()
            .find(|s| s.block_name == block_name && s.binary_version == version)
    }

    pub fn best_version(&self, block_name: &str) -> Option<&StabilityScore> {
        self.scores
            .iter()
            .filter(|s| s.block_name == block_name)
            .max_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    pub fn worst_score(&self) -> Option<&StabilityScore> {
        self.scores.iter().min_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    pub fn count(&self) -> usize {
        self.scores.len()
    }

    pub fn clear(&mut self) {
        self.scores.clear();
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
    fn test_stability_score_lifecycle() {
        let mut s = StabilityScore::new("block_a", "1.0.0");
        assert_eq!(s.score, 1.0);
        s.record_crash();
        assert_eq!(s.score, 0.9);
        s.record_uptime(10_000);
        assert_eq!(s.score, 0.91);
    }

    #[test]
    fn test_score_floor() {
        let mut s = StabilityScore::new("block", "1.0");
        for _ in 0..20 {
            s.record_crash();
        }
        assert!(s.score >= 0.0);
    }

    #[test]
    fn test_best_version() {
        let mut store = StabilityStore::new();
        store.record(StabilityScore::new("block", "1.0"));
        store.record(StabilityScore::new("block", "2.0"));
        store.scores[0].score = 0.5;
        store.scores[1].score = 0.9;
        let best = store.best_version("block").unwrap();
        assert_eq!(best.binary_version, "2.0");
    }

    #[test]
    fn test_upsert() {
        let mut store = StabilityStore::new();
        store.record(StabilityScore::new("block", "1.0"));
        store.record(StabilityScore::new("block", "1.0"));
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn test_is_healthy() {
        let mut s = StabilityScore::new("b", "1");
        assert!(s.is_healthy());
        s.score = 0.3;
        assert!(!s.is_healthy());
    }
}
