//! Fault tolerance & recovery contract shared between the fault interceptor
//! (Ring 0 / kernel and the host process manager) and the recovery dialog
//! (Ring 3 GUI).
//!
//! When a task faults — a caught panic in the host runtime, a hung task, or a
//! protected CPU fault in a kernel task — the interceptor freezes that task
//! (the scheduler stops giving it CPU time) and publishes a [`FaultInfo`] on
//! the IPC bus. The interactive crash dialog then answers with a
//! [`RecoveryAction`] (`Rollback` / `Terminate` / `Resume`), which the fault
//! manager applies to the frozen task.
//!
//! The types here carry no logic so they can be reused by the `std` workspace
//! crates and mirrored by the `no_std` kernel without pulling extra
//! dependencies.

use serde::{Deserialize, Serialize};

/// The action chosen by the user (or an automated policy) for a faulted task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryAction {
    /// Restore the task's saved [`crate`]-level metadata (priority, memory
    /// quota, owning block) from its pre-run snapshot and resume it.
    Rollback,
    /// Terminate the faulted task and release its resources.
    Terminate,
    /// Clear the fault and resume the task from where it stopped.
    Resume,
}

impl RecoveryAction {
    /// Stable, human-readable name (used in console/log output).
    pub fn name(self) -> &'static str {
        match self {
            RecoveryAction::Rollback => "rollback",
            RecoveryAction::Terminate => "terminate",
            RecoveryAction::Resume => "resume",
        }
    }
}

/// The class of fault captured by the interceptor, so the UI can explain what
/// went wrong and pick sensible defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultKind {
    /// A Rust panic escaped the task body and was caught by the interceptor.
    Panic,
    /// The task exceeded its time budget / stopped making progress.
    Hang,
    /// A protected CPU exception (page fault, general protection, ...) was
    /// taken while a task was running.
    TaskFault,
}

impl FaultKind {
    /// Stable, human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            FaultKind::Panic => "panic",
            FaultKind::Hang => "hang",
            FaultKind::TaskFault => "fault",
        }
    }
}

/// A fault raised by the interceptor and delivered to the recovery dialog.
///
/// This is intentionally lightweight — it carries only identity and a short
/// message, never a memory image of the task (the expensive state stays in the
/// task's own [`TaskSnapshot`](crate)). It is the payload of the
/// [`Payload::FaultReport`](crate::ipc_protocol::Payload::FaultReport) IPC
/// message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultInfo {
    /// Process/task id that faulted.
    pub pid: u64,
    /// Human-readable task name.
    pub name: String,
    /// What kind of fault was taken.
    pub kind: FaultKind,
    /// Short, user-facing description (panic message / exception name).
    pub message: String,
    /// Wall-clock timestamp (ms since the Unix epoch, or a monotonic kernel
    /// tick count in the kernel) so the UI can order multiple faults.
    pub timestamp_ms: u64,
}

impl FaultInfo {
    /// Build a fault report.
    pub fn new(
        pid: u64,
        name: impl Into<String>,
        kind: FaultKind,
        message: impl Into<String>,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            pid,
            name: name.into(),
            kind,
            message: message.into(),
            timestamp_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_action_names_are_stable() {
        assert_eq!(RecoveryAction::Rollback.name(), "rollback");
        assert_eq!(RecoveryAction::Terminate.name(), "terminate");
        assert_eq!(RecoveryAction::Resume.name(), "resume");
    }

    #[test]
    fn fault_info_roundtrips_through_bincode() {
        let info = FaultInfo::new(7, "worker", FaultKind::Panic, "boom", 123);
        let bytes = bincode::serialize(&info).unwrap();
        let back: FaultInfo = bincode::deserialize(&bytes).unwrap();
        assert_eq!(info, back);
    }

    #[test]
    fn fault_kind_names_are_stable() {
        assert_eq!(FaultKind::Panic.name(), "panic");
        assert_eq!(FaultKind::Hang.name(), "hang");
        assert_eq!(FaultKind::TaskFault.name(), "fault");
    }
}
