use std::collections::VecDeque;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct FlightRecorder {
    buffer: VecDeque<FlightEvent>,
    max_events: usize,
    retention_secs: u64,
    total_overwritten: u64,
}

#[derive(Debug, Clone)]
pub struct FlightEvent {
    pub timestamp_ms: u128,
    pub kind: EventKind,
    pub message: String,
    pub data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Info,
    Warning,
    Error,
    Panic,
    Syscall,
    IPC,
    BlockLifecycle,
    Heartbeat,
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EventKind::Info => write!(f, "INFO"),
            EventKind::Warning => write!(f, "WARN"),
            EventKind::Error => write!(f, "ERROR"),
            EventKind::Panic => write!(f, "PANIC"),
            EventKind::Syscall => write!(f, "SYSCALL"),
            EventKind::IPC => write!(f, "IPC"),
            EventKind::BlockLifecycle => write!(f, "BLOCK"),
            EventKind::Heartbeat => write!(f, "HB"),
        }
    }
}

impl FlightRecorder {
    pub fn new(max_events: usize, retention_secs: u64) -> Self {
        Self {
            buffer: VecDeque::with_capacity(max_events + 1),
            max_events,
            retention_secs,
            total_overwritten: 0,
        }
    }

    pub fn record(&mut self, kind: EventKind, message: &str) {
        let now = now_ms();
        self.evict_old(now);

        if self.buffer.len() >= self.max_events {
            self.buffer.pop_front();
            self.total_overwritten += 1;
        }

        self.buffer.push_back(FlightEvent {
            timestamp_ms: now,
            kind,
            message: message.to_string(),
            data: None,
        });
    }

    pub fn record_with_data(&mut self, kind: EventKind, message: &str, data: &str) {
        let now = now_ms();
        self.evict_old(now);

        if self.buffer.len() >= self.max_events {
            self.buffer.pop_front();
            self.total_overwritten += 1;
        }

        self.buffer.push_back(FlightEvent {
            timestamp_ms: now,
            kind,
            message: message.to_string(),
            data: Some(data.to_string()),
        });
    }

    pub fn dump(&self) -> Vec<FlightEvent> {
        self.buffer.iter().cloned().collect()
    }

    pub fn dump_since(&self, since_ms: u128) -> Vec<FlightEvent> {
        self.buffer
            .iter()
            .filter(|e| e.timestamp_ms >= since_ms)
            .cloned()
            .collect()
    }

    pub fn dump_by_kind(&self, kind: EventKind) -> Vec<FlightEvent> {
        self.buffer
            .iter()
            .filter(|e| e.kind == kind)
            .cloned()
            .collect()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn total_overwritten(&self) -> u64 {
        self.total_overwritten
    }

    pub fn to_string(&self) -> String {
        let mut output = String::from("=== Flight Recorder Dump ===\n");
        for event in &self.buffer {
            let ts = event.timestamp_ms;
            output.push_str(&format!("[{ts:16}] [{:5}] {}", event.kind, event.message));
            if let Some(data) = &event.data {
                output.push_str(&format!(" | data={data}"));
            }
            output.push('\n');
        }
        output.push_str(&format!(
            "=== Total: {} events, {} overwritten ===\n",
            self.buffer.len(),
            self.total_overwritten
        ));
        output
    }

    fn evict_old(&mut self, now: u128) {
        let cutoff = now - (self.retention_secs as u128 * 1000);
        while let Some(front) = self.buffer.front() {
            if front.timestamp_ms < cutoff {
                self.buffer.pop_front();
            } else {
                break;
            }
        }
    }
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
    fn test_flight_recorder_empty() {
        let fr = FlightRecorder::new(100, 60);
        assert!(fr.is_empty());
        assert_eq!(fr.len(), 0);
    }

    #[test]
    fn test_record_and_dump() {
        let mut fr = FlightRecorder::new(100, 60);
        fr.record(EventKind::Info, "system started");
        fr.record(EventKind::Heartbeat, "hb ok");
        assert_eq!(fr.len(), 2);
        let dump = fr.dump();
        assert_eq!(dump.len(), 2);
        assert_eq!(dump[0].message, "system started");
    }

    #[test]
    fn test_record_with_data() {
        let mut fr = FlightRecorder::new(100, 60);
        fr.record_with_data(EventKind::IPC, "packet sent", "block_id=5");
        let dump = fr.dump();
        assert_eq!(dump[0].data, Some("block_id=5".into()));
    }

    #[test]
    fn test_max_events() {
        let mut fr = FlightRecorder::new(5, 60);
        for i in 0..10 {
            fr.record(EventKind::Info, &format!("event-{i}"));
        }
        assert_eq!(fr.len(), 5);
        assert_eq!(fr.total_overwritten(), 5);
        assert_eq!(fr.dump()[0].message, "event-5");
    }

    #[test]
    fn test_dump_by_kind() {
        let mut fr = FlightRecorder::new(100, 60);
        fr.record(EventKind::Info, "info1");
        fr.record(EventKind::Error, "error1");
        fr.record(EventKind::Info, "info2");
        let errors = fr.dump_by_kind(EventKind::Error);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_clear() {
        let mut fr = FlightRecorder::new(100, 60);
        fr.record(EventKind::Info, "test");
        assert!(!fr.is_empty());
        fr.clear();
        assert!(fr.is_empty());
    }

    #[test]
    fn test_to_string() {
        let mut fr = FlightRecorder::new(100, 60);
        fr.record(EventKind::Panic, "kernel panic");
        let s = fr.to_string();
        assert!(s.contains("PANIC"));
        assert!(s.contains("kernel panic"));
    }
}
