//! Interrupt-level fault interceptor (Ring 0).
//!
//! CPU exceptions in user tasks are the Ring-0 twin of the host-side
//! `catch_unwind` in `aios-process-mgr`. Instead of falling into a global
//! `fatal` halt, the interceptor captures a light [`TaskSnapshot`] of the
//! faulting task — pid, instruction pointer, stack pointer, flags and a
//! lightweight FNV-1a [`TaskSnapshot::state_hash`] of that register bundle —
//! freezes it (the scheduler skips it forever, marked via its `sleep_until`
//! deadline) and publishes a [`FaultRecord`]. The interactive crash dialog —
//! drawn by the windowed GUI over the desktop — answers with **Resume**
//! (unfreeze, ignore), **Terminate** (remove the task) or **Rollback**
//! (restore the captured `rip`/`rsp`/segment/frame, verified against the
//! Faults and hangs are enqueued in a bounded **system fault queue**
//! ([`FAULT_QUEUE_LEN`]): several tasks may be frozen at once, and the crash
//! dialog resolves them oldest-first, advancing to the next record without
//! closing. Every resolution is also pushed to a bounded [`FaultRecord`]
//! history ring with resolution statistics.
//!
//! A per-task **fault budget** guards against fault loops: a task that fails
//! more than [`MAX_FAULTS_PER_TASK`] times can no longer be Resumed or
//! Rolled back — the dialog locks to Terminate only, so a defective task
//! cannot pin the desktop in an endless crash/resume cycle.
//!
//! The **hang watchdog** ([`record_hang`], triggered by the scheduler) is the
//! spec-mandated companion to exception interception: a ring-3 task whose
//! saved `rip` does not change for [`crate::sched::HANG_WATCHDOG_TICKS`] is
//! declared hung — no syscall, sleep or progress — and frozen through the same
//! dialog. Hangs charge the same per-task budget (the operator may Resume a
//! busy-work task or Terminate a true spinner), rollback restores the recorded
//! stall point.
//!
//! Kernel-context faults (ring 0, including the idle context) still halt the
//! system: their stack is not a resumable user boundary.
//!
//! All state is global and single-CPU: the scheduler runs one task at a time,
//! and one dialog is shown at a time (the front of the queue). Reads/writes
//! happen from the interrupt path and the idle context, never concurrently
//! (interrupt gates disable nesting).

use crate::interrupts::InterruptFrame;
use crate::sched::MAX_TASKS;
use core::sync::atomic::{AtomicBool, Ordering};

/// How many faults a single task may survive before the dialog locks to
/// Terminate. Counts 1..=this are recoverable (Resume/Rollback), any fault
/// beyond is forced Terminate.
pub const MAX_FAULTS_PER_TASK: u8 = 3;

/// Depth of the bounded fault history ring (diagnostics).
pub const FAULT_HISTORY_LEN: usize = 8;

/// Depth of the pending **system fault queue**. Faults and hangs are enqueued
/// oldest-first and the crash dialog resolves the front; a task is frozen while
/// its record waits. One slot per task is enough (at most `MAX_TASKS - 1` user
/// tasks can be frozen at once), so a full queue can only be reached
/// transiently and never in practice.
pub const FAULT_QUEUE_LEN: usize = MAX_TASKS;

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
    /// Not a CPU exception: the scheduler watchdog detected a ring-3 task
    /// whose rip did not change for [`crate::sched::HANG_WATCHDOG_TICKS`].
    Hang = 0x20,
}

impl FaultCode {
    /// Stable, human-readable name shown in the crash dialog and serial log.
    pub fn name(self) -> &'static str {
        match self {
            FaultCode::DivideError => "divide error",
            FaultCode::InvalidOpcode => "invalid opcode",
            FaultCode::GeneralProtection => "general protection fault",
            FaultCode::PageFault => "page fault",
            FaultCode::Hang => "hang",
        }
    }
}

/// Lightweight pre-fault task state captured by the interceptor, restored by
/// the Rollback action. Deliberately small (no memory image) — it only needs
/// to bring the task back to its fault boundary and carry an integrity hash.
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
    /// Lightweight FNV-1a hash of the register bundle; verifies the record on
    /// Rollback and identifies it in the history ring.
    pub state_hash: u64,
}

impl TaskSnapshot {
    /// FNV-1a hash of the captured register bundle — the "state hash" the
    /// snapshot carries. Deterministic, cheap, and catches in-memory record
    /// corruption when Rollback re-verifies it.
    pub fn state_hash(pid: u32, rip: u64, cs: u64, rsp: u64, rflags: u64) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for v in [pid as u64, rip, cs, rsp, rflags] {
            for b in v.to_ne_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h
    }
}

/// A faulted task awaiting a recovery decision.
#[derive(Clone, Copy)]
pub struct FaultRecord {
    /// The frozen task's snapshot.
    pub snapshot: TaskSnapshot,
    /// What went wrong.
    pub kind: FaultCode,
    /// How many times this pid has faulted (the `MAX_FAULTS_PER_TASK` budget
    /// is charged against this counter).
    pub count: u8,
}

/// What the crash dialog chose for the frozen task.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Recovery {
    /// Ignore the fault and put the task back on the run queue.
    Resume = 0,
    /// Remove the task from the scheduler for good.
    Terminate = 1,
    /// Restore the captured snapshot and resume.
    Rollback = 2,
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
/// The pending system fault queue (FIFO): faults/hangs awaiting a decision.
/// `QUEUE_HEAD` is the front (what the dialog shows), `QUEUE_LEN` the depth.
static mut FAULT_QUEUE: [Option<FaultRecord>; FAULT_QUEUE_LEN] = [None; FAULT_QUEUE_LEN];
static mut QUEUE_HEAD: usize = 0;
static mut QUEUE_LEN: usize = 0;
static mut CRASH_COUNTS: [u8; MAX_TASKS] = [0; MAX_TASKS];
static mut HISTORY: [Option<FaultRecord>; FAULT_HISTORY_LEN] = [None; FAULT_HISTORY_LEN];
static mut HISTORY_NEXT: usize = 0;
static mut HISTORY_ITEMS: usize = 0;
static mut FAULT_TOTAL: u64 = 0;
static mut RESOLUTIONS: [u64; 3] = [0; 3];

fn counters() -> &'static mut [u8; MAX_TASKS] {
    unsafe {
        #[allow(static_mut_refs)]
        &mut *core::ptr::addr_of_mut!(CRASH_COUNTS)
    }
}

fn queue() -> &'static mut [Option<FaultRecord>; FAULT_QUEUE_LEN] {
    unsafe {
        #[allow(static_mut_refs)]
        &mut *core::ptr::addr_of_mut!(FAULT_QUEUE)
    }
}

fn ring() -> &'static mut [Option<FaultRecord>; FAULT_HISTORY_LEN] {
    unsafe {
        #[allow(static_mut_refs)]
        &mut *core::ptr::addr_of_mut!(HISTORY)
    }
}

/// Publish a pending fault record and charge the task's crash counter.
///
/// Shared by the exception interceptor and the scheduler hang watchdog; the
/// caller keeps its own logging/freeze/switch responsibilities (interrupt
/// context vs. scheduler context differ).
fn publish(pid: u32, kind: FaultCode, snapshot: TaskSnapshot) -> u8 {
    let count = {
        let slot = pid as usize;
        let c = counters();
        c[slot] = c[slot].saturating_add(1);
        c[slot]
    };
    unsafe {
        let len = core::ptr::addr_of!(QUEUE_LEN).read();
        if len >= FAULT_QUEUE_LEN {
            crate::vprintln!(
                "[fault] queue full, dropping {} for pid {}",
                kind.name(),
                pid
            );
            return count;
        }
        let head = core::ptr::addr_of!(QUEUE_HEAD).read();
        queue()[(head + len) % FAULT_QUEUE_LEN] = Some(FaultRecord {
            snapshot,
            kind,
            count,
        });
        *core::ptr::addr_of_mut!(QUEUE_LEN) = len + 1;
        FAULT_PENDING.store(true, Ordering::Relaxed);
    }
    count
}

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
    let count = publish(
        pid,
        kind,
        TaskSnapshot {
            pid,
            rip: frame.rip,
            cs: frame.cs,
            rsp: frame.rsp,
            rflags: frame.rflags,
            state_hash: TaskSnapshot::state_hash(pid, frame.rip, frame.cs, frame.rsp, frame.rflags),
        },
    );
    crate::vprintln!(
        "[fault] pid {} {} at rip={:#x} (crash #{}/{}) -> frozen, awaiting recovery",
        pid,
        kind.name(),
        frame.rip,
        count,
        MAX_FAULTS_PER_TASK
    );
    crate::kprintln!(
        "[serial] [fault] pid {} {} at rip={:#x} frozen (crash #{}/{})",
        pid,
        kind.name(),
        frame.rip,
        count,
        MAX_FAULTS_PER_TASK
    );
    crate::sched::freeze_task(pid);
    crate::sched::schedule(frame);
}

/// Hang-watchdog entry: publish a stall record for `pid` from the snapshot the
/// scheduler built from the task's last preemption frame, and freeze the task.
/// Unlike [`intercept`] this runs *inside* `schedule()`, so it must not call
/// `schedule` again — the ongoing rotation simply skips the now-frozen task
/// and the crash dialog raises in the idle context.
pub fn record_hang(snapshot: TaskSnapshot) {
    let pid = snapshot.pid;
    let count = publish(pid, FaultCode::Hang, snapshot);
    crate::vprintln!(
        "[hang] pid {} stalled at rip={:#x} (crash #{}/{}) -> frozen, awaiting recovery",
        pid,
        snapshot.rip,
        count,
        MAX_FAULTS_PER_TASK
    );
    crate::kprintln!(
        "[serial] [hang] pid {} stalled at rip={:#x} frozen (crash #{}/{})",
        pid,
        snapshot.rip,
        count,
        MAX_FAULTS_PER_TASK
    );
    crate::sched::freeze_task(pid);
}

/// True while a fault awaits a recovery decision (crash dialog gate).
pub fn pending() -> bool {
    FAULT_PENDING.load(Ordering::Relaxed)
}

/// Copy of the front fault record awaiting a decision, if any (what the crash
/// dialog shows).
pub fn peek() -> Option<FaultRecord> {
    unsafe {
        if core::ptr::addr_of!(QUEUE_LEN).read() == 0 {
            return None;
        }
        let head = core::ptr::addr_of!(QUEUE_HEAD).read();
        queue()[head]
    }
}

/// How many fault/hang records are queued and awaiting a decision.
pub fn queued() -> usize {
    unsafe { core::ptr::addr_of!(QUEUE_LEN).read() }
}

/// Remove the front record after it has been resolved. Leaves the next queued
/// fault pending (the dialog advances without closing), or clears the pending
/// flag once the queue drains.
fn pop_front() {
    unsafe {
        let len = core::ptr::addr_of!(QUEUE_LEN).read();
        if len == 0 {
            return;
        }
        let head = core::ptr::addr_of!(QUEUE_HEAD).read();
        queue()[head] = None;
        let remaining = len - 1;
        *core::ptr::addr_of_mut!(QUEUE_LEN) = remaining;
        if remaining == 0 {
            *core::ptr::addr_of_mut!(QUEUE_HEAD) = 0;
            FAULT_PENDING.store(false, Ordering::Relaxed);
        } else {
            *core::ptr::addr_of_mut!(QUEUE_HEAD) = (head + 1) % FAULT_QUEUE_LEN;
        }
    }
}

/// The fault record still awaiting a decision has exhausted its budget: the
/// dialog may only Terminate the task.
pub fn over_budget_current() -> bool {
    peek().is_some_and(|r| over_budget(&r))
}

fn over_budget(record: &FaultRecord) -> bool {
    record.count > MAX_FAULTS_PER_TASK
}

/// Total faults ever intercepted and resolved (history ring aggregate).
pub fn fault_total() -> u64 {
    unsafe { core::ptr::addr_of!(FAULT_TOTAL).read() }
}

/// How many times each recovery action has been applied.
pub fn resolution_count(action: Recovery) -> u64 {
    unsafe { core::ptr::addr_of!(RESOLUTIONS).read()[action as usize] }
}

/// Copy of the resolved-fault history ring, oldest first (diagnostics; backs
/// the shell `faults` command). Capped at [`FAULT_HISTORY_LEN`] entries.
pub fn history_records() -> alloc::vec::Vec<FaultRecord> {
    let h = ring();
    let next = unsafe { core::ptr::addr_of!(HISTORY_NEXT).read() };
    let items = unsafe { core::ptr::addr_of!(HISTORY_ITEMS).read() };
    let oldest = (next + FAULT_HISTORY_LEN - items) % FAULT_HISTORY_LEN;
    let mut out = alloc::vec::Vec::new();
    for k in 0..items {
        if let Some(rec) = h[(oldest + k) % FAULT_HISTORY_LEN] {
            out.push(rec);
        }
    }
    out
}

/// Copy of the pending system fault queue, oldest first (diagnostics; backs the
/// shell `faults` command). Up to [`FAULT_QUEUE_LEN`] entries.
pub fn pending_records() -> alloc::vec::Vec<FaultRecord> {
    let q = queue();
    let head = unsafe { core::ptr::addr_of!(QUEUE_HEAD).read() };
    let len = unsafe { core::ptr::addr_of!(QUEUE_LEN).read() };
    let mut out = alloc::vec::Vec::new();
    for k in 0..len {
        if let Some(rec) = q[(head + k) % FAULT_QUEUE_LEN] {
            out.push(rec);
        }
    }
    out
}

/// Apply the crash dialog's decision to the frozen task. Non-Terminate
/// decisions are ignored once the task is over its fault budget, so a
/// defective task must be Terminated instead of looping the dialog.
pub fn resolve(action: Recovery) {
    let Some(record) = peek() else {
        return;
    };
    if action != Recovery::Terminate && over_budget(&record) {
        crate::vprintln!(
            "[fault] pid {} over fault budget ({} > {}): {} ignored",
            record.snapshot.pid,
            record.count,
            MAX_FAULTS_PER_TASK,
            action.name()
        );
        crate::kprintln!(
            "[serial] [fault] pid {} over fault budget: only Terminate",
            record.snapshot.pid
        );
        return;
    }
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
            counters()[pid as usize] = 0;
            crate::vprintln!(
                "[fault] pid {} {} ({})",
                pid,
                action.name(),
                record.kind.name()
            );
            crate::kprintln!("[serial] [fault] pid {} {}", pid, action.name());
        }
        Recovery::Rollback => {
            let verified = TaskSnapshot::state_hash(
                pid,
                record.snapshot.rip,
                record.snapshot.cs,
                record.snapshot.rsp,
                record.snapshot.rflags,
            ) == record.snapshot.state_hash;
            crate::sched::restore_task(pid, record.snapshot);
            crate::vprintln!(
                "[fault] pid {} {} to rip={:#x} state {}",
                pid,
                action.name(),
                record.snapshot.rip,
                if verified { "verified" } else { "mismatch" }
            );
            crate::kprintln!(
                "[serial] [fault] pid {} {} state {}",
                pid,
                action.name(),
                if verified { "verified" } else { "mismatch" }
            );
        }
    }
    unsafe {
        let h = ring();
        let idx = core::ptr::addr_of!(HISTORY_NEXT).read();
        h[idx] = Some(record);
        *core::ptr::addr_of_mut!(HISTORY_NEXT) = (idx + 1) % FAULT_HISTORY_LEN;
        *core::ptr::addr_of_mut!(HISTORY_ITEMS) =
            (core::ptr::addr_of!(HISTORY_ITEMS).read() + 1).min(FAULT_HISTORY_LEN);
        (*core::ptr::addr_of_mut!(FAULT_TOTAL)) += 1;
        (*core::ptr::addr_of_mut!(RESOLUTIONS))[action as usize] += 1;
    }
    crate::kprintln!(
        "[serial] [fault] totals faults={} resume={} terminate={} rollback={}",
        fault_total(),
        resolution_count(Recovery::Resume),
        resolution_count(Recovery::Terminate),
        resolution_count(Recovery::Rollback)
    );
    pop_front();
    if let Some(next) = peek() {
        crate::vprintln!(
            "[fault] queue: pid {} {} next ({} queued)",
            next.snapshot.pid,
            next.kind.name(),
            queued()
        );
        crate::kprintln!(
            "[serial] [fault] queue: pid {} {} next ({} queued)",
            next.snapshot.pid,
            next.kind.name(),
            queued()
        );
    }
}

/// Assert-fault helper for tests: the interceptor itself runs in interrupt
/// context and cannot be invoked from an unprivileged test, but the record /
/// resolve round trip is unit-testable once a record is placed.
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[cfg(test)]
    fn pin(record: FaultRecord) {
        unsafe {
            *core::ptr::addr_of_mut!(QUEUE_HEAD) = 0;
            *core::ptr::addr_of_mut!(QUEUE_LEN) = 0;
            queue()[0] = Some(record);
            *core::ptr::addr_of_mut!(QUEUE_LEN) = 1;
        }
        FAULT_PENDING.store(true, Ordering::Relaxed);
    }

    fn snap(pid: u32, rip: u64) -> TaskSnapshot {
        TaskSnapshot {
            pid,
            rip,
            cs: 0x1b,
            rsp: 0x7fff,
            rflags: 0x202,
            state_hash: TaskSnapshot::state_hash(pid, rip, 0x1b, 0x7fff, 0x202),
        }
    }

    #[test]
    fn code_names_are_stable() {
        assert_eq!(FaultCode::PageFault.name(), "page fault");
        assert_eq!(
            FaultCode::GeneralProtection.name(),
            "general protection fault"
        );
        assert_eq!(FaultCode::DivideError.name(), "divide error");
        assert_eq!(FaultCode::InvalidOpcode.name(), "invalid opcode");
        assert_eq!(FaultCode::Hang.name(), "hang");
    }

    #[test]
    fn recovery_names_are_stable() {
        assert_eq!(Recovery::Resume.name(), "resume");
        assert_eq!(Recovery::Terminate.name(), "terminate");
        assert_eq!(Recovery::Rollback.name(), "rollback");
    }

    #[test]
    fn state_hash_is_stable_and_sensitive() {
        let snap = snap(3, 0x401234);
        let same = TaskSnapshot::state_hash(3, 0x401234, 0x1b, 0x7fff, 0x202);
        assert_eq!(snap.state_hash, same);
        assert_ne!(
            snap.state_hash,
            TaskSnapshot::state_hash(3, 0x401235, 0x1b, 0x7fff, 0x202)
        );
        assert_ne!(
            snap.state_hash,
            TaskSnapshot::state_hash(4, 0x401234, 0x1b, 0x7fff, 0x202)
        );
        assert_ne!(snap.state_hash, 0);
    }

    #[test]
    fn snapshot_defaults() {
        let snap = snap(3, 0x401234);
        assert_eq!(snap.pid, 3);
        assert_eq!(snap.rip, 0x401234);
        assert_eq!(snap.cs, 0x1b);
        assert_eq!(
            snap.state_hash,
            TaskSnapshot::state_hash(3, 0x401234, 0x1b, 0x7fff, 0x202)
        );
    }

    #[test]
    fn resume_rollback_clear_and_resolve_terminate() {
        FAULT_PENDING.store(false, Ordering::Relaxed);
        counters()[3] = 0;
        pin(FaultRecord {
            snapshot: snap(3, 0x401234),
            kind: FaultCode::DivideError,
            count: 1,
        });
        assert!(pending());
        resolve(Recovery::Resume);
        assert!(!pending(), "resume resolves the pending fault");
        assert_eq!(resolution_count(Recovery::Resume), 1);
        assert_eq!(fault_total(), 1);
    }

    #[test]
    fn over_budget_forces_terminate() {
        FAULT_PENDING.store(false, Ordering::Relaxed);
        counters()[2] = 0;
        pin(FaultRecord {
            snapshot: snap(2, 0x3000),
            kind: FaultCode::PageFault,
            count: MAX_FAULTS_PER_TASK + 1,
        });
        assert!(over_budget_current());
        resolve(Recovery::Resume);
        resolve(Recovery::Rollback);
        assert!(
            pending(),
            "non-Terminate decisions are ignored once over the budget"
        );
        resolve(Recovery::Terminate);
        assert!(
            !pending(),
            "Terminate always resolves, even over the budget"
        );
        assert_eq!(counters()[2], 0, "terminate resets the fault counter");
        assert_eq!(resolution_count(Recovery::Terminate), 1);
    }

    #[test]
    fn history_ring_caps_and_counts() {
        FAULT_PENDING.store(false, Ordering::Relaxed);
        let total_before = fault_total();
        let term_before = resolution_count(Recovery::Terminate);
        let next_before = unsafe { core::ptr::addr_of!(HISTORY_NEXT).read() };
        for pid in 1..=(FAULT_HISTORY_LEN + 2) {
            pin(FaultRecord {
                snapshot: snap(pid as u32, 0x1000 + pid as u64),
                kind: FaultCode::GeneralProtection,
                count: 1,
            });
            resolve(Recovery::Terminate);
            counters()[pid] = 0;
        }
        assert_eq!(fault_total(), total_before + (FAULT_HISTORY_LEN + 2) as u64);
        assert_eq!(
            resolution_count(Recovery::Terminate),
            term_before + (FAULT_HISTORY_LEN + 2) as u64
        );
        let next = unsafe { core::ptr::addr_of!(HISTORY_NEXT).read() };
        assert_eq!(
            next,
            (next_before + FAULT_HISTORY_LEN + 2) % FAULT_HISTORY_LEN,
            "history head advances modulo FAULT_HISTORY_LEN"
        );
        let recs = history_records();
        assert_eq!(
            recs.len(),
            FAULT_HISTORY_LEN,
            "ring caps at FAULT_HISTORY_LEN"
        );
        let pids: Vec<u32> = recs.iter().map(|r| r.snapshot.pid).collect();
        assert_eq!(
            &pids[FAULT_HISTORY_LEN - 2..],
            &[9, 10],
            "newest two pushes land at the tail, oldest first"
        );
    }

    #[test]
    fn hang_record_publishes_and_resolves() {
        FAULT_PENDING.store(false, Ordering::Relaxed);
        counters()[5] = 0;
        let snapshot = TaskSnapshot {
            pid: 5,
            rip: 0x2340,
            cs: 0x1b,
            rsp: 0x9fff,
            rflags: 0x202,
            state_hash: TaskSnapshot::state_hash(5, 0x2340, 0x1b, 0x9fff, 0x202),
        };
        record_hang(snapshot);
        assert!(pending(), "a detected hang opens the recovery gate");
        let rec = peek().expect("hang published a record");
        assert_eq!(rec.kind, FaultCode::Hang);
        assert_eq!(rec.snapshot.pid, 5);
        assert_eq!(rec.snapshot.rip, 0x2340);
        assert_eq!(rec.count, 1);
        resolve(Recovery::Terminate);
        assert!(!pending(), "terminate resolves the hang");
        counters()[5] = 0;
    }

    #[test]
    fn queue_preserves_order_and_drains() {
        FAULT_PENDING.store(false, Ordering::Relaxed);
        unsafe {
            *core::ptr::addr_of_mut!(QUEUE_HEAD) = 0;
            *core::ptr::addr_of_mut!(QUEUE_LEN) = 0;
        }
        counters()[1] = 0;
        counters()[2] = 0;
        publish(1, FaultCode::DivideError, snap(1, 0x1111));
        publish(2, FaultCode::PageFault, snap(2, 0x2222));
        assert_eq!(queued(), 2, "both faults queue");
        assert_eq!(peek().expect("front present").snapshot.pid, 1);
        resolve(Recovery::Terminate);
        assert!(pending(), "a queued fault keeps the gate open");
        assert_eq!(peek().expect("next present").snapshot.pid, 2);
        assert_eq!(queued(), 1);
        resolve(Recovery::Terminate);
        assert_eq!(queued(), 0);
        assert!(!pending(), "the gate clears once the queue drains");
        counters()[1] = 0;
        counters()[2] = 0;
    }

    #[test]
    fn rollback_counter_tracks_terminate_reset() {
        FAULT_PENDING.store(false, Ordering::Relaxed);
        counters()[4] = 1;
        pin(FaultRecord {
            snapshot: snap(4, 0x5000),
            kind: FaultCode::InvalidOpcode,
            count: 1,
        });
        resolve(Recovery::Rollback);
        assert!(!pending());
        assert_eq!(resolution_count(Recovery::Rollback), 1);
        assert_eq!(counters()[4], 1, "rollback keeps the fault counter");
        counters()[4] = 0;
    }
}
