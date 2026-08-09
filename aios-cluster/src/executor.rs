//! Process executors that run remote spawns on a node.
//!
//! A coordinator node without an executor is a pure scheduler; a node with an
//! executor can host processes requested by peers. [`SchedulerProcessExecutor`]
//! bridges to the real `aios-process-mgr` scheduler, [`MockProcessExecutor`]
//! provides a deterministic stand-in for tests.
use crate::types::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Interface a node uses to actually run and control processes.
pub trait ProcessExecutor: Send + Sync {
    /// Spawn a process described by `spec`; returns the local process id.
    fn spawn(&self, spec: &RemoteProcessSpec) -> Result<u64, String>;
    /// Terminate the process `pid`.
    fn kill(&self, pid: u64) -> Result<(), String>;
    /// Change the priority of process `pid`.
    fn set_priority(&self, pid: u64, priority: u8) -> Result<(), String>;
    /// Snapshot of all processes hosted on this node.
    fn status(&self) -> Vec<RemoteProcessStatus>;
    /// Load snapshot for node metrics.
    fn metrics(&self) -> NodeMetrics;
    /// Extract an opaque state snapshot of process `pid` for migration.
    fn extract_state(&self, pid: u64) -> Result<Vec<u8>, String>;
    /// Restore an extracted state snapshot into process `pid`.
    fn restore_state(&self, pid: u64, state: &[u8]) -> Result<(), String>;
}

/// Deterministic in-memory executor for tests and mock deployments.
#[derive(Default)]
pub struct MockProcessExecutor {
    node_id: NodeId,
    processes: Arc<Mutex<HashMap<u64, MockProcess>>>,
    states: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
    next_pid: AtomicU64,
}

#[derive(Debug, Clone)]
struct MockProcess {
    name: String,
    priority: u8,
    ram_mb: u64,
    state: String,
}

impl MockProcessExecutor {
    /// Create an executor hosting processes on behalf of `node_id`.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            processes: Arc::new(Mutex::new(HashMap::new())),
            states: Arc::new(Mutex::new(HashMap::new())),
            next_pid: AtomicU64::new(1),
        }
    }
}

impl ProcessExecutor for MockProcessExecutor {
    fn spawn(&self, spec: &RemoteProcessSpec) -> Result<u64, String> {
        let pid = self.next_pid.fetch_add(1, Ordering::Relaxed);
        self.processes.lock().unwrap().insert(
            pid,
            MockProcess {
                name: spec.name.clone(),
                priority: spec.priority,
                ram_mb: spec.ram_mb,
                state: "Running".into(),
            },
        );
        // The init payload seeds the process state snapshot, so tests can
        // inject migration state through the spawn spec.
        self.states
            .lock()
            .unwrap()
            .insert(pid, spec.payload.clone());
        Ok(pid)
    }

    fn kill(&self, pid: u64) -> Result<(), String> {
        match self.processes.lock().unwrap().remove(&pid) {
            Some(_) => {
                self.states.lock().unwrap().remove(&pid);
                Ok(())
            }
            None => Err(format!("no process with pid {pid}")),
        }
    }

    fn set_priority(&self, pid: u64, priority: u8) -> Result<(), String> {
        let mut guard = self.processes.lock().unwrap();
        match guard.get_mut(&pid) {
            Some(proc) => {
                proc.priority = priority.clamp(0, 4);
                Ok(())
            }
            None => Err(format!("no process with pid {pid}")),
        }
    }

    fn status(&self) -> Vec<RemoteProcessStatus> {
        let guard = self.processes.lock().unwrap();
        let mut out: Vec<RemoteProcessStatus> = guard
            .iter()
            .map(|(pid, proc)| RemoteProcessStatus {
                id: RemoteProcessId {
                    node: self.node_id,
                    pid: *pid,
                },
                name: proc.name.clone(),
                state: proc.state.clone(),
                ram_mb: proc.ram_mb,
            })
            .collect();
        out.sort_by_key(|s| s.id.pid);
        out
    }

    fn metrics(&self) -> NodeMetrics {
        let guard = self.processes.lock().unwrap();
        let ram_used: u64 = guard.values().map(|p| p.ram_mb).sum();
        NodeMetrics::new(0.0, ram_used, 16384, guard.len() as u64)
    }

    fn extract_state(&self, pid: u64) -> Result<Vec<u8>, String> {
        let guard = self.states.lock().unwrap();
        match guard.get(&pid) {
            Some(state) => Ok(state.clone()),
            None => Err(format!("no process with pid {pid}")),
        }
    }

    fn restore_state(&self, pid: u64, state: &[u8]) -> Result<(), String> {
        let mut guard = self.states.lock().unwrap();
        match guard.get_mut(&pid) {
            Some(slot) => {
                slot.clear();
                slot.extend_from_slice(state);
                Ok(())
            }
            None => Err(format!("no process with pid {pid}")),
        }
    }
}

/// Executor bridging to the real `aios-process-mgr` scheduler.
pub struct SchedulerProcessExecutor {
    node_id: NodeId,
    scheduler: Arc<Mutex<aios_process_mgr::scheduler::Scheduler>>,
    states: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
}

impl SchedulerProcessExecutor {
    /// Wrap a scheduler that runs processes for `node_id`.
    pub fn new(
        node_id: NodeId,
        scheduler: Arc<Mutex<aios_process_mgr::scheduler::Scheduler>>,
    ) -> Self {
        Self {
            node_id,
            scheduler,
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl ProcessExecutor for SchedulerProcessExecutor {
    fn spawn(&self, spec: &RemoteProcessSpec) -> Result<u64, String> {
        let pid = self
            .scheduler
            .lock()
            .unwrap()
            .spawn_process(
                &spec.name,
                aios_process_mgr::task::Priority::from_u8(spec.priority),
                spec.ram_mb,
            )
            .map_err(|e| e.to_string())?;
        self.states
            .lock()
            .unwrap()
            .insert(pid.0, spec.payload.clone());
        Ok(pid.0)
    }

    fn kill(&self, pid: u64) -> Result<(), String> {
        self.scheduler
            .lock()
            .unwrap()
            .kill_process(aios_process_mgr::task::ProcessId(pid))
            .map(|_| {
                self.states.lock().unwrap().remove(&pid);
            })
            .map_err(|e| e.to_string())
    }

    fn set_priority(&self, pid: u64, priority: u8) -> Result<(), String> {
        self.scheduler
            .lock()
            .unwrap()
            .set_priority(
                aios_process_mgr::task::ProcessId(pid),
                aios_process_mgr::task::Priority::from_u8(priority),
            )
            .map_err(|e| e.to_string())
    }

    fn status(&self) -> Vec<RemoteProcessStatus> {
        self.scheduler
            .lock()
            .unwrap()
            .all_processes()
            .into_iter()
            .map(|p| RemoteProcessStatus {
                id: RemoteProcessId {
                    node: self.node_id,
                    pid: p.pid.0,
                },
                name: p.name.clone(),
                state: format!("{:?}", p.state),
                ram_mb: p.ram_quota_mb,
            })
            .collect()
    }

    fn metrics(&self) -> NodeMetrics {
        let sched = self.scheduler.lock().unwrap();
        let (used, total) = sched.ram_usage();
        NodeMetrics::new(0.0, used, total, sched.process_count() as u64)
    }

    fn extract_state(&self, pid: u64) -> Result<Vec<u8>, String> {
        let guard = self.states.lock().unwrap();
        match guard.get(&pid) {
            Some(state) => Ok(state.clone()),
            None => Err(format!("no process with pid {pid}")),
        }
    }

    fn restore_state(&self, pid: u64, state: &[u8]) -> Result<(), String> {
        let mut guard = self.states.lock().unwrap();
        match guard.get_mut(&pid) {
            Some(slot) => {
                slot.clear();
                slot.extend_from_slice(state);
                Ok(())
            }
            None => Err(format!("no process with pid {pid}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_executor_lifecycle() {
        let exec = MockProcessExecutor::new(7);
        let spec = RemoteProcessSpec::new("db", 2, 128);
        let pid = exec.spawn(&spec).unwrap();
        assert_eq!(pid, 1);
        assert_eq!(exec.status().len(), 1);
        assert_eq!(exec.metrics().ram_used_mb, 128);
        assert_eq!(exec.metrics().process_count, 1);

        exec.set_priority(pid, 0).unwrap();
        assert_eq!(
            exec.set_priority(999, 0),
            Err("no process with pid 999".into())
        );

        exec.kill(pid).unwrap();
        assert!(exec.status().is_empty());
        assert_eq!(exec.kill(pid), Err("no process with pid 1".into()));
    }

    #[test]
    fn mock_state_roundtrip() {
        let exec = MockProcessExecutor::new(7);
        let pid = exec
            .spawn(&RemoteProcessSpec::new("db", 2, 64).with_payload(b"seed-state".to_vec()))
            .unwrap();
        assert_eq!(exec.extract_state(pid).unwrap(), b"seed-state");

        exec.restore_state(pid, b"migrated-state").unwrap();
        assert_eq!(exec.extract_state(pid).unwrap(), b"migrated-state");

        assert_eq!(
            exec.extract_state(999),
            Err("no process with pid 999".into())
        );
        assert_eq!(
            exec.restore_state(999, b"x"),
            Err("no process with pid 999".into())
        );

        exec.kill(pid).unwrap();
        assert_eq!(exec.extract_state(pid), Err("no process with pid 1".into()));
    }
}
