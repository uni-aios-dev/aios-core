//! Fault interceptor for the host-side process manager.
//!
//! Real processes run on OS threads. The scheduler wraps each payload with
//! [`std::panic::catch_unwind`] so a panicking task cannot take down the
//! scheduler or the whole runtime — instead the panic is captured, the task is
//! frozen (marked [`ProcessState::Crashed`](crate::task::ProcessState::Crashed)
//! and suspended) and a [`FaultInfo`] is queued for the interactive recovery
//! dialog.
//!
//! A lightweight [`TaskSnapshot`] of the task's scheduling metadata is taken
//! when it is spawned, so the user can choose *rollback* (restore the snapshot
//! and run again), *terminate* (drop the task) or *resume* (ignore the error).

use crate::task::{Priority, Process, ProcessId};
use aios_core::fault::{FaultInfo, FaultKind};
use std::cell::Cell;
use std::sync::Once;

thread_local! {
    /// Set while a task payload runs inside [`run_isolated`]; the global panic
    /// hook stays silent on such threads so a caught panic is not also printed
    /// to stderr.
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

fn install_quiet_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let quiet = QUIET.with(|q| q.get());
            if !quiet {
                default(info);
            }
        }));
    });
}

/// Turn a caught panic payload into a short, user-facing message.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "task panicked".to_string()
    }
}

/// Run a task payload, catching any panic. Returns `Some(message)` if the
/// payload panicked, `None` if it completed (or stopped) normally.
///
/// The default panic hook is suppressed for the duration so a recovered fault
/// does not spam the console.
pub fn run_isolated<F: FnOnce()>(f: F) -> Option<String> {
    install_quiet_hook();
    QUIET.with(|q| q.set(true));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    QUIET.with(|q| q.set(false));
    result.err().map(panic_message)
}

/// Metadata snapshot of a task taken when it is spawned, used by the
/// `Rollback` recovery action.
///
/// This is deliberately *light* — it captures only scheduling metadata, never
/// the task's memory image. Heavier state transfer is the block/state
/// subsystem's job.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskSnapshot {
    /// Process id.
    pub pid: ProcessId,
    /// Task name.
    pub name: String,
    /// Scheduling priority at spawn.
    pub priority: Priority,
    /// RAM quota in MiB.
    pub ram_quota_mb: u64,
    /// Owning block, if any.
    pub block_id: Option<u32>,
    /// Crash count at snapshot time (normally 0).
    pub crash_count: u32,
}

impl TaskSnapshot {
    /// Capture the scheduling metadata of `process`.
    pub fn capture(process: &Process) -> Self {
        Self {
            pid: process.pid,
            name: process.name.clone(),
            priority: process.priority,
            ram_quota_mb: process.ram_quota_mb,
            block_id: process.block_id,
            crash_count: process.crash_count,
        }
    }

    /// Restore this snapshot's metadata onto `process` (used by rollback).
    pub fn restore_to(&self, process: &mut Process) {
        process.priority = self.priority;
        process.ram_quota_mb = self.ram_quota_mb;
        process.block_id = self.block_id;
        process.crash_count = self.crash_count;
    }
}

/// Build a [`FaultInfo`] for a faulted task.
pub fn fault_for(process: &Process, kind: FaultKind, message: String) -> FaultInfo {
    FaultInfo::new(
        process.pid.0,
        process.name.clone(),
        kind,
        message,
        crate::task::now_ms(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_capture_and_restore() {
        let mut p = Process::new(ProcessId::new(9), "svc".into(), Priority::High, 128);
        p.crash_count = 2;
        let snap = TaskSnapshot::capture(&p);
        assert_eq!(snap.priority, Priority::High);
        assert_eq!(snap.ram_quota_mb, 128);
        assert_eq!(snap.crash_count, 2);

        p.priority = Priority::Background;
        p.ram_quota_mb = 4;
        p.crash_count = 7;
        snap.restore_to(&mut p);
        assert_eq!(p.priority, Priority::High);
        assert_eq!(p.ram_quota_mb, 128);
        assert_eq!(p.crash_count, 2);
    }

    #[test]
    fn run_isolated_catches_panic() {
        let msg = run_isolated(|| panic!("kaboom"));
        assert_eq!(msg.as_deref(), Some("kaboom"));
    }

    #[test]
    fn run_isolated_returns_none_on_success() {
        let msg = run_isolated(|| {});
        assert!(msg.is_none());
    }

    #[test]
    fn run_isolated_captures_string_payload() {
        let msg = run_isolated(|| std::panic::panic_any(String::from("owned")));
        assert_eq!(msg.as_deref(), Some("owned"));
    }
}
