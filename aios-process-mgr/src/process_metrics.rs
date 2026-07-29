use crate::task::ProcessId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct ProcessMetricsInner {
    pub pid: ProcessId,
    pub messages_sent: AtomicU64,
    pub messages_received: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    pub errors: AtomicU64,
    pub syscall_count: AtomicU64,
    pub wakeups: AtomicU64,
}

impl ProcessMetricsInner {
    pub fn new(pid: ProcessId) -> Self {
        Self {
            pid,
            messages_sent: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            syscall_count: AtomicU64::new(0),
            wakeups: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessMetrics {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub errors: u64,
    pub syscall_count: u64,
    pub wakeups: u64,
}

pub struct ProcessMetricsStore {
    metrics: HashMap<ProcessId, Arc<ProcessMetricsInner>>,
}

thread_local! {
    static CURRENT_PID: std::cell::Cell<Option<ProcessId>> = const { std::cell::Cell::new(None) };
}

pub fn bind_current_thread(pid: ProcessId) {
    CURRENT_PID.with(|cell| cell.set(Some(pid)));
}

pub fn current_pid() -> Option<ProcessId> {
    CURRENT_PID.with(|cell| cell.get())
}

pub fn record_sent(bytes: u64) {
    if let Some(pid) = current_pid() {
        if let Some(store) = ProcessMetricsStore::global() {
            if let Some(m) = store.get(pid) {
                m.messages_sent.fetch_add(1, Ordering::Relaxed);
                m.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
            }
        }
    }
}

pub fn record_received(bytes: u64) {
    if let Some(pid) = current_pid() {
        if let Some(store) = ProcessMetricsStore::global() {
            if let Some(m) = store.get(pid) {
                m.messages_received.fetch_add(1, Ordering::Relaxed);
                m.bytes_received.fetch_add(bytes, Ordering::Relaxed);
            }
        }
    }
}

pub fn record_error() {
    if let Some(pid) = current_pid() {
        if let Some(store) = ProcessMetricsStore::global() {
            if let Some(m) = store.get(pid) {
                m.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub fn record_syscall() {
    if let Some(pid) = current_pid() {
        if let Some(store) = ProcessMetricsStore::global() {
            if let Some(m) = store.get(pid) {
                m.syscall_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub fn record_wakeup() {
    if let Some(pid) = current_pid() {
        if let Some(store) = ProcessMetricsStore::global() {
            if let Some(m) = store.get(pid) {
                m.wakeups.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

use std::sync::OnceLock;

static GLOBAL_STORE: OnceLock<Mutex<ProcessMetricsStore>> = OnceLock::new();

impl ProcessMetricsStore {
    pub fn init() {
        GLOBAL_STORE
            .set(Mutex::new(ProcessMetricsStore::new()))
            .ok();
    }

    pub fn global() -> Option<std::sync::MutexGuard<'static, ProcessMetricsStore>> {
        GLOBAL_STORE.get().and_then(|m| m.lock().ok())
    }

    pub fn new() -> Self {
        Self {
            metrics: HashMap::new(),
        }
    }
}

impl Default for ProcessMetricsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessMetricsStore {
    pub fn register(&mut self, pid: ProcessId) -> Arc<ProcessMetricsInner> {
        let inner = Arc::new(ProcessMetricsInner::new(pid));
        self.metrics.insert(pid, inner.clone());
        inner
    }

    pub fn unregister(&mut self, pid: ProcessId) {
        self.metrics.remove(&pid);
    }

    pub fn get(&self, pid: ProcessId) -> Option<Arc<ProcessMetricsInner>> {
        self.metrics.get(&pid).cloned()
    }

    pub fn snapshot(&self, pid: ProcessId) -> Option<ProcessMetrics> {
        self.metrics.get(&pid).map(|m| ProcessMetrics {
            messages_sent: m.messages_sent.load(Ordering::Relaxed),
            messages_received: m.messages_received.load(Ordering::Relaxed),
            bytes_sent: m.bytes_sent.load(Ordering::Relaxed),
            bytes_received: m.bytes_received.load(Ordering::Relaxed),
            errors: m.errors.load(Ordering::Relaxed),
            syscall_count: m.syscall_count.load(Ordering::Relaxed),
            wakeups: m.wakeups.load(Ordering::Relaxed),
        })
    }

    pub fn snapshot_all(&self) -> HashMap<ProcessId, ProcessMetrics> {
        self.metrics
            .iter()
            .map(|(&pid, m)| {
                (
                    pid,
                    ProcessMetrics {
                        messages_sent: m.messages_sent.load(Ordering::Relaxed),
                        messages_received: m.messages_received.load(Ordering::Relaxed),
                        bytes_sent: m.bytes_sent.load(Ordering::Relaxed),
                        bytes_received: m.bytes_received.load(Ordering::Relaxed),
                        errors: m.errors.load(Ordering::Relaxed),
                        syscall_count: m.syscall_count.load(Ordering::Relaxed),
                        wakeups: m.wakeups.load(Ordering::Relaxed),
                    },
                )
            })
            .collect()
    }

    pub fn clear(&mut self) {
        self.metrics.clear();
    }

    pub fn count(&self) -> usize {
        self.metrics.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_register_and_snapshot() {
        let mut store = ProcessMetricsStore::new();
        let pid = ProcessId::new(1);
        store.register(pid);
        assert_eq!(store.count(), 1);

        let snap = store.snapshot(pid).unwrap();
        assert_eq!(snap.messages_sent, 0);

        let inner = store.get(pid).unwrap();
        inner.messages_sent.fetch_add(5, Ordering::Relaxed);
        inner.bytes_sent.fetch_add(1024, Ordering::Relaxed);

        let snap = store.snapshot(pid).unwrap();
        assert_eq!(snap.messages_sent, 5);
        assert_eq!(snap.bytes_sent, 1024);
    }

    #[test]
    fn test_store_unregister() {
        let mut store = ProcessMetricsStore::new();
        let pid = ProcessId::new(1);
        store.register(pid);
        store.unregister(pid);
        assert_eq!(store.count(), 0);
        assert!(store.snapshot(pid).is_none());
    }

    #[test]
    fn test_store_snapshot_all() {
        let mut store = ProcessMetricsStore::new();
        store.register(ProcessId::new(1));
        store.register(ProcessId::new(2));
        let all = store.snapshot_all();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_thread_local_bind_and_record() {
        let mut store = ProcessMetricsStore::new();
        let pid = ProcessId::new(42);
        store.register(pid);

        ProcessMetricsStore::init();

        {
            let mut global = ProcessMetricsStore::global().unwrap();
            global.register(pid);
        }

        bind_current_thread(pid);
        assert_eq!(current_pid(), Some(pid));

        record_sent(100);
        record_received(200);
        record_error();
        record_syscall();
        record_wakeup();

        let global = ProcessMetricsStore::global().unwrap();
        let snap = global.snapshot(pid).unwrap();
        assert_eq!(snap.messages_sent, 1);
        assert_eq!(snap.bytes_sent, 100);
        assert_eq!(snap.messages_received, 1);
        assert_eq!(snap.bytes_received, 200);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.syscall_count, 1);
        assert_eq!(snap.wakeups, 1);

        drop(global);
        bind_current_thread(ProcessId::new(0));
    }

    #[test]
    fn test_store_clear() {
        let mut store = ProcessMetricsStore::new();
        store.register(ProcessId::new(1));
        store.register(ProcessId::new(2));
        store.clear();
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn test_metrics_atomic_independence() {
        let mut store = ProcessMetricsStore::new();
        let p1 = ProcessId::new(1);
        let p2 = ProcessId::new(2);
        store.register(p1);
        store.register(p2);

        let m1 = store.get(p1).unwrap();
        let m2 = store.get(p2).unwrap();

        m1.messages_sent.fetch_add(10, Ordering::Relaxed);
        m2.messages_sent.fetch_add(20, Ordering::Relaxed);

        assert_eq!(store.snapshot(p1).unwrap().messages_sent, 10);
        assert_eq!(store.snapshot(p2).unwrap().messages_sent, 20);
    }

    #[test]
    fn test_record_without_binding_is_noop() {
        ProcessMetricsStore::init();
        bind_current_thread(ProcessId::new(999));
        record_sent(50);
        record_error();

        let global = ProcessMetricsStore::global().unwrap();
        assert!(global.snapshot(ProcessId::new(999)).is_none());
        drop(global);

        bind_current_thread(ProcessId::new(0));
    }
}
