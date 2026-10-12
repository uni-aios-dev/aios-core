//! Interrupt-level fault interceptor (Ring 0).
//!
//! CPU exceptions in user tasks are the Ring-0 twin of the host-side
//! `catch_unwind` in `aios-process-mgr`. Instead of falling into a global
//! `fatal` halt, the interceptor captures a light [`TaskSnapshot`] of the
//! faulting task, freezes it (the scheduler skips it forever, marked via its
//! `sleep_until` deadline) and publishes a [`FaultRecord`]. The interactive
//! crash dialog — drawn by the windowed GUI over the desktop — answers with
//! **Resume** (unfreeze, ignore), **Terminate** (remove the task) or
//! **Rollback** (restore the captured `rip`/`rsp`/segment/frame and unfreeze).
//! Kernel-context faults (ring 0, including the idle context) still halt the
//! system: their stack is not a resumable user boundary.
//!
//! All state is global and single-slot: this kernel schedules one CPU, and one
//! dialog is shown at a time. Reads/writes happen from the interrupt path and
//! the idle context, never concurrently (interrupt gates disable nesting).

use crate::interrupts::InterruptFrame;
use core::sync::atomic::{AtomicBool, Ordering};

/// The CPU exception that froze the task (message source for the dialog).
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum FaultCode {
    /// Vector 0 — divide-by-zero in the task.
    DivideError = 0,
    /// Vector 6 — invalid opcode in the task.
    InvalidOpcode = 6,
    /// Vector 13 — general protection fault (bad descriptor/segment access).
    GeneralProtection = 13,
    /// Vector 14 — page fault (unmapped/protected access).
    PageFault = 14,
}

impl FaultCode {
    /// Stable, human-readable name shown in the crash dialog and serial log.
    pub fn name(self) -> &'static str {
        match self {
            FaultCode::DivideError => "divide error",
            FaultCode::InvalidOpcode => "invalid opcode",
            FaultCode::GeneralProtection => "general protection fault",
            FaultCode::PageFault => "page fault",
        }
    }
}

/// Lightweight pre-fault task state captured by the interceptor, restored by
/// the Rollback action. Deliberately small (no memory image) — it only needs
/// to bring the task back to its fault boundary.
#[derive(Clone, Copy)]
pub struct TaskSnapshot {
    /// Task slot (pid maps 1:1 to the slot).
    pub pid: u32,
    /// Instruction pointer at the fault.
    pub rip: u64,
    /// Code segment at the fault (always the user segment for rescued tasks).
    pub cs: u64,
    /// Stack pointer at the fault.
    pub rsp: u64,
    /// Flags at the fault.
    pub rflags: u64,
}

/// A faulted task awaiting a recovery decision.
#[derive(Clone, Copy)]
pub struct FaultRecord {
    /// The frozen task's snapshot.
    pub snapshot: TaskSnapshot,
    /// What went wrong.
    pub kind: FaultCode,
}

/// What the crash dialog chose for the frozen task.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Ignore the fault and put the task back on the run queue.
    Resume,
    /// Remove the task from the scheduler for good.
    Terminate,
    /// Restore the captured snapshot and resume.
    Rollback,
}

impl Recovery {
    /// Stable name for logging.
    pub fn name(self) -> &'static str {
        match self {
            Recovery::Resume => "resume",
            Recovery::Terminate => "terminate",
            Recovery::Rollback => "rollback",
        }
    }
}

static FAULT_PENDING: AtomicBool = AtomicBool::new(false);
static mut FAULT: Option<FaultRecord> = None;

/// Freeze the faulting current task and publish its fault record, then switch
/// away from it so the recovery dialog (running in the idle context) can make
/// a decision. `fatal` for ring-0 faults.
pub fn intercept(frame: &mut InterruptFrame, kind: FaultCode) {
    // Ring-0 context (kernel thread or idle) shares one stack with the kernel;
    // there is no resumable user boundary, so the classic halt is kept.
    if frame.cs == crate::gdt::KERNEL_CS as u64 {
        crate::interrupts::fatal_with(0x7000_0000 + kind as u32, kind.name());
    }
    let pid = crate::sched::current_pid();
    crate::vprintln!(
        "[fault] pid {} {} at rip={:#x} -> frozen, awaiting recovery",
        pid,
        kind.name(),
        frame.rip
    );
    crate::kprintln!(
        "[serial] [fault] pid {} {} at rip={:#x} frozen",
        pid,
        kind.name(),
        frame.rip
    );
    unsafe {
        *core::ptr::addr_of_mut!(FAULT) = Some(FaultRecord {
            snapshot: TaskSnapshot {
                pid,
                rip: frame.rip,
                cs: frame.cs,
                rsp: frame.rsp,
                rflags: frame.rflags,
            },
            kind,
        });
        FAULT_PENDING.store(true, Ordering::Relaxed);
    }
    crate::sched::freeze_task(pid);
    crate::sched::schedule(frame);
}

/// True while a fault awaits a recovery decision (crash dialog gate).
pub fn pending() -> bool {
    FAULT_PENDING.load(Ordering::Relaxed)
}

/// Copy of the pending fault record, if any.
pub fn peek() -> Option<FaultRecord> {
    unsafe { core::ptr::addr_of!(FAULT).read_volatile() }
}

/// Clear the pending fault record.
fn clear() {
    unsafe {
        *core::ptr::addr_of_mut!(FAULT) = None;
    }
    FAULT_PENDING.store(false, Ordering::Relaxed);
}

/// Apply the crash dialog's decision to the frozen task.
pub fn resolve(action: Recovery) {
    let Some(record) = peek() else {
        return;
    };
    let pid = record.snapshot.pid;
    match action {
        Recovery::Resume => {
            crate::sched::unfreeze_task(pid);
            crate::vprintln!(
                "[fault] pid {} {}, ignoring {}",
                pid,
                action.name(),
                record.kind.name()
            );
            crate::kprintln!("[serial] [fault] pid {} {}", pid, action.name());
        }
        Recovery::Terminate => {
            crate::sched::remove_task(pid);
            crate::vprintln!(
                "[fault] pid {} {} ({})",
                pid,
                action.name(),
                record.kind.name()
            );
            crate::kprintln!("[serial] [fault] pid {} {}", pid, action.name());
        }
        Recovery::Rollback => {
            crate::sched::restore_task(pid, record.snapshot);
            crate::vprintln!(
                "[fault] pid {} {} to rip={:#x}",
                pid,
                action.name(),
                record.snapshot.rip
            );
            crate::kprintln!("[serial] [fault] pid {} {}", pid, action.name());
        }
    }
    clear();
}

/// Assert-fault helper for tests: the interceptor itself runs in interrupt
/// context and cannot be invoked from an unprivileged test, but the record /
/// resolve round trip is unit-testable once a record is placed.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_names_are_stable() {
        assert_eq!(FaultCode::PageFault.name(), "page fault");
        assert_eq!(
            FaultCode::GeneralProtection.name(),
            "general protection fault"
        );
        assert_eq!(FaultCode::DivideError.name(), "divide error");
        assert_eq!(FaultCode::InvalidOpcode.name(), "invalid opcode");
    }

    #[test]
    fn recovery_names_are_stable() {
        assert_eq!(Recovery::Resume.name(), "resume");
        assert_eq!(Recovery::Terminate.name(), "terminate");
        assert_eq!(Recovery::Rollback.name(), "rollback");
    }

    #[test]
    fn snapshot_defaults() {
        let snap = TaskSnapshot {
            pid: 3,
            rip: 0x401234,
            cs: 0x1b,
            rsp: 0x7fff,
            rflags: 0x202,
        };
        assert_eq!(snap.pid, 3);
        assert_eq!(snap.rip, 0x401234);
        assert_eq!(snap.cs, 0x1b);
    }
}
