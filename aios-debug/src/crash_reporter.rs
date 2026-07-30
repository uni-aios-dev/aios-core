use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashReport {
    pub id: String,
    pub timestamp_ms: u128,
    pub kind: CrashKind,
    pub thread_name: String,
    pub message: String,
    pub stack_hash: String,
    pub module_info: Vec<ModuleInfo>,
    pub flight_recorder_snippet: String,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CrashKind {
    Panic,
    WatchdogTimeout,
    OOM,
    BlockCrash,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleInfo {
    pub name: String,
    pub version: String,
    pub sha256_prefix: String,
}

pub struct CrashReporter {
    app_name: String,
    app_version: String,
    reports: Vec<CrashReport>,
}

impl CrashReporter {
    pub fn new(app_name: &str, app_version: &str) -> Self {
        Self {
            app_name: app_name.to_string(),
            app_version: app_version.to_string(),
            reports: Vec::new(),
        }
    }

    pub fn generate_report(
        &mut self,
        kind: CrashKind,
        thread_name: &str,
        message: &str,
        stack_trace: &str,
        flight_recorder_dump: &str,
        zero_knowledge: bool,
    ) -> CrashReport {
        let id = format!("CRASH-{}-{:016x}", self.app_name, rand_seed());

        let module_info = vec![ModuleInfo {
            name: self.app_name.clone(),
            version: self.app_version.clone(),
            sha256_prefix: compute_hash_prefix(message),
        }];

        let kind_debug = format!("{:?}", &kind);
        let report = CrashReport {
            id: id.clone(),
            timestamp_ms: now_ms(),
            kind,
            thread_name: thread_name.to_string(),
            message: if zero_knowledge {
                hash_string(message)
            } else {
                message.to_string()
            },
            stack_hash: compute_hash_prefix(stack_trace),
            module_info,
            flight_recorder_snippet: if zero_knowledge {
                String::new()
            } else {
                flight_recorder_dump.to_string()
            },
            redacted: zero_knowledge,
        };

        self.reports.push(report.clone());
        log::error!("Crash report generated: {id} (kind={kind_debug}, zk={zero_knowledge})");
        report
    }

    pub fn reports(&self) -> &[CrashReport] {
        &self.reports
    }

    pub fn report_count(&self) -> usize {
        self.reports.len()
    }

    pub fn latest_report(&self) -> Option<&CrashReport> {
        self.reports.last()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.reports).unwrap_or_default()
    }
}

fn hash_string(s: &str) -> String {
    let hash = sha2::Sha256::digest(s.as_bytes());
    hex::encode(hash)
}

fn compute_hash_prefix(s: &str) -> String {
    let hash = sha2::Sha256::digest(s.as_bytes());
    hex::encode(&hash[..4])
}

fn rand_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
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
    fn test_report_generation() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");
        let report = cr.generate_report(
            CrashKind::Panic,
            "main",
            "index out of bounds",
            "stack trace line 1\nline 2",
            "=== Flight Recorder ===",
            false,
        );
        assert!(report.id.starts_with("CRASH-aios-core-"));
        assert!(!report.redacted);
        assert!(report.flight_recorder_snippet.contains("Flight Recorder"));
    }

    #[test]
    fn test_report_count() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");
        assert_eq!(cr.report_count(), 0);
        cr.generate_report(CrashKind::Unknown, "t1", "msg", "stack", "fr", false);
        assert_eq!(cr.report_count(), 1);
    }

    #[test]
    fn test_zero_knowledge_report() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");
        let report = cr.generate_report(
            CrashKind::WatchdogTimeout,
            "watchdog",
            "sensitive data: user=admin",
            "stack",
            "fr data",
            true,
        );
        assert!(report.redacted);
        assert_ne!(report.message, "sensitive data: user=admin");
        assert!(report.flight_recorder_snippet.is_empty());
    }

    #[test]
    fn test_latest_report() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");
        assert!(cr.latest_report().is_none());
        cr.generate_report(CrashKind::OOM, "t1", "oom", "s", "fr", false);
        assert!(cr.latest_report().is_some());
    }

    #[test]
    fn test_to_json() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");
        cr.generate_report(
            CrashKind::BlockCrash,
            "b1",
            "block failed",
            "st",
            "fr",
            false,
        );
        let json = cr.to_json();
        assert!(json.contains("BlockCrash"));
        assert!(json.contains("block failed"));
    }

    #[test]
    fn test_crash_panic_caught_and_reported() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("intentional test panic: index out of bounds");
        }));

        assert!(result.is_err());

        let report = cr.generate_report(
            CrashKind::Panic,
            "test-thread",
            "intentional test panic: index out of bounds",
            "stack:0xdeadbeef\nstack:0xcafebabe",
            "=== flight recorder dump ===",
            false,
        );

        assert_eq!(report.kind, CrashKind::Panic);
        assert!(report.id.starts_with("CRASH-aios-core-"));
        assert!(report.message.contains("intentional test panic"));
        assert_eq!(cr.report_count(), 1);
        assert!(cr.latest_report().is_some());
    }

    #[test]
    fn test_crash_oom_redacted_report() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");

        let report = cr.generate_report(
            CrashKind::OOM,
            "memory-monitor",
            "OOM in block 'data_processor': allocation failed (requested 4294967296 bytes)",
            "stack:0xabcd",
            "fr: oom at 2026-07-29T14:00:00Z",
            true,
        );

        assert!(report.redacted);
        assert_eq!(report.kind, CrashKind::OOM);
        assert!(!report.message.contains("4294967296"));
        assert!(report.flight_recorder_snippet.is_empty());
    }

    #[test]
    fn test_crash_multiple_reports_order() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");

        let r1 = cr.generate_report(CrashKind::Panic, "t1", "first", "s1", "fr1", false);
        let r2 = cr.generate_report(CrashKind::OOM, "t2", "second", "s2", "fr2", false);
        let r3 = cr.generate_report(
            CrashKind::WatchdogTimeout,
            "t3",
            "third",
            "s3",
            "fr3",
            false,
        );

        assert_eq!(cr.report_count(), 3);
        let reports = cr.reports();
        assert_eq!(reports[0].message, "first");
        assert_eq!(reports[1].message, "second");
        assert_eq!(reports[2].message, "third");

        assert!(r1.timestamp_ms <= r2.timestamp_ms);
        assert!(r2.timestamp_ms <= r3.timestamp_ms);
    }

    #[test]
    fn test_crash_unknown_kind_defaults() {
        let mut cr = CrashReporter::new("aios-core", "1.0.0");
        let report = cr.generate_report(
            CrashKind::Unknown,
            "unknown",
            "something unexpected happened",
            "",
            "",
            false,
        );
        assert_eq!(report.kind, CrashKind::Unknown);
        assert!(!report.redacted);
        assert!(report.flight_recorder_snippet.is_empty());
    }
}
