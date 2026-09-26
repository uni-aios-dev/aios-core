//! Preemptive round-robin scheduler (Milestones 3+5), hybrid switching.
//!
//! Tasks are full trap-frame snapshots ([`InterruptFrame`]); switching means
//! saving the frame captured by an interrupt into the outgoing task and
//! overwriting the trap frame on the stack with the incoming task's saved
//! state, so the final `iretq` resumes (or enters) the chosen task — ring 0
//! tasks resume inside their interrupted code, ring 3 tasks are entered
//! through a fabricated user-mode frame (CS=0x1B / SS=0x23).
//!
//! Ring-3 tasks are preemptive: the PIT may switch them at any interrupt.
//! Ring-0 kernel threads switch only cooperatively through [`yield_kernel`]
//! (`int 0xfa`); a hardware IRQ refreshes their saved frame state but never
//! switches them away, so every resume lands at a known, rsp-stable
//! instruction instead of inside transient formatting code.
//!
//! Milestone 5 adds a per-task `sleep_until` deadline: a sleeping task is
//! skipped by the selection loop and stops consuming CPU; when its deadline
//! passes it is woken in place. With every user task asleep the rotation
//! naturally falls back to task slot 0 (the idle context), which is logged as
//! `[sched] idle`.

use crate::gdt::{KERNEL_CS, USER_CS, USER_DS};
use crate::interrupts::InterruptFrame;
use crate::interrupts::{TICKS, TIMER_HZ};
use crate::{kprintln, vprintln};
use core::sync::atomic::Ordering;

extern "C" {
    /// Resumes a ring-0 task from its saved frame: restores all general
    /// registers, switches to the task's own kernel stack and `iretq`'s.
    /// Never returns. `rdi` holds the frame pointer on entry.
    #[allow(dead_code)]
    fn aios_restore_ring0(frame: *const InterruptFrame) -> !;
    /// Resumes a ring-3 task: restores registers and does `iretq` with
    /// proper SS:RSP on the stack for the privilege transition.
    /// Never returns. `rdi` holds the frame pointer on entry.
    #[allow(dead_code)]
    fn aios_restore_ring3(frame: *const InterruptFrame) -> !;
}

/// Switch cadence: preemption fires 4 times per second.
const SWITCH_DIVIDER: u64 = TIMER_HZ / 4;

/// One slot is always reserved for the CPU-wide idle task (slot 0).
pub const MAX_TASKS: usize = 5;

#[derive(Clone, Copy)]
struct Task {
    present: bool,
    /// Set once the task's frame holds real state (boot task captures its
    /// frame on the first switch away; spawned frames start fabricated).
    valid_frame: bool,
    /// Tick at which the task may run again; `0` = runnable now.
    sleep_until: u64,
    frame: InterruptFrame,
}

impl Task {
    const fn empty() -> Self {
        Self {
            present: false,
            valid_frame: false,
            sleep_until: 0,
            frame: InterruptFrame {
                r15: 0,
                r14: 0,
                r13: 0,
                r12: 0,
                r11: 0,
                r10: 0,
                r9: 0,
                r8: 0,
                rdi: 0,
                rsi: 0,
                rbp: 0,
                rbx: 0,
                rdx: 0,
                rcx: 0,
                ds: 0,
                rax: 0,
                vector: 0,
                error_code: 0,
                rip: 0,
                cs: 0,
                rflags: 0,
                rsp: 0,
                ss: 0,
            },
        }
    }
}

static mut TASKS: [Task; MAX_TASKS] = [
    Task::empty(),
    Task::empty(),
    Task::empty(),
    Task::empty(),
    Task::empty(),
];

/// Index of the currently scheduled task (`0` = boot/idle context).
static mut CURRENT: isize = -1;
static mut LAST_SWITCH_TICK: u64 = 0;
static mut SWITCH_COUNT: u64 = 0;
/// True until <code>kernel_main</code> hands off to <code>idle_loop</code>.
///
/// While set, schedule() refuses to capture or switch the boot context (slot
/// 0): its frame would snapshot mid-startup code whose kernel-stack region is
/// reused once idle_loop begins, making a later "resume" jump into clobbered
/// stack frames.
static mut BOOT_ACTIVE: bool = true;

/// Marks the end of boot: re-anchors slot 0 to a fresh `idle_loop` entry on
/// its own dedicated kernel stack and deregisters the boot context so its
/// stack frames are never resumed. Call right before `kernel_main` parks
/// itself in an idle loop.
pub fn boot_finished() {
    unsafe {
        #[allow(static_mut_refs)]
        {
            let idle_top = IDLE_STACK.as_ptr() as usize as u64 + IDLE_STACK.len() as u64;
            let t = tasks();
            t[0].present = true;
            t[0].valid_frame = true;
            t[0].frame.rip = crate::idle_loop as *const () as u64;
            t[0].frame.cs = 0x08;
            t[0].frame.ss = 0x10;
            t[0].frame.ds = 0x10;
            t[0].frame.rsp = idle_top;
            t[0].frame.rflags = 0x202;
            CURRENT = -1;
            BOOT_ACTIVE = false;
        }
    }
}

/// Reserved region below the boot context's stack: idle runs here once boot
/// hands off, so its saved frames never overlap abandoned startup frames.
const IDLE_STACK_SIZE_BYTES: usize = 32 * 1024;
static mut IDLE_STACK: [u8; IDLE_STACK_SIZE_BYTES] = [0; IDLE_STACK_SIZE_BYTES];

fn tasks() -> &'static mut [Task; MAX_TASKS] {
    unsafe {
        #[allow(static_mut_refs)]
        &mut *core::ptr::addr_of_mut!(TASKS)
    }
}

pub fn init() {
    unsafe {
        let t = tasks();
        t[0].present = true;
        t[0].valid_frame = false;
        CURRENT = 0;
        LAST_SWITCH_TICK = 0;
    }
    vprintln!(
        "[sched] ready (idle=task0, round-robin every {} ticks)",
        SWITCH_DIVIDER
    );
    kprintln!("[serial] [sched] ready");
}

/// Register a kernel thread that starts at `entry` running on `stack_top`.
pub fn spawn_kernel(entry: u64, stack_top: u64) -> Option<usize> {
    spawn_with(|frame| {
        frame.rip = entry;
        frame.cs = 0x08;
        frame.ss = 0x10;
        frame.ds = 0x10;
        frame.rsp = stack_top;
        frame.rflags = 0x202;
    })
}

/// Register a ring-3 task starting at `entry` with `stack_top` (both must be
/// user-mapped pages prepared by `user::spawn_program`).
pub fn spawn_user(entry: u64, stack_top: u64, arg1: u64, arg2: u64) -> Option<usize> {
    spawn_with(|frame| {
        frame.rip = entry;
        frame.cs = USER_CS as u64;
        frame.ss = USER_DS as u64;
        frame.ds = USER_DS as u64;
        frame.rsp = stack_top;
        frame.rflags = 0x202;
        frame.rdi = arg1;
        frame.rsi = arg2;
    })
}

fn spawn_with(fill: impl FnOnce(&mut InterruptFrame)) -> Option<usize> {
    let t = tasks();
    for (slot, task) in t.iter_mut().enumerate().skip(1) {
        if !task.present {
            fill(&mut task.frame);
            task.present = true;
            task.valid_frame = true;
            return Some(slot);
        }
    }
    None
}

/// Id of the task whose syscall is being serviced (0 = idle/kernel itself).
pub fn current_pid() -> u32 {
    unsafe {
        #[allow(static_mut_refs)]
        {
            CURRENT.max(0) as u32
        }
    }
}

pub fn switch_count() -> u64 {
    unsafe {
        #[allow(static_mut_refs)]
        {
            SWITCH_COUNT
        }
    }
}

/// True when the slot holds a registered task (`pid` maps 1:1 to the slot).
pub fn task_present(pid: u32) -> bool {
    let idx = pid as usize;
    idx > 0 && idx < MAX_TASKS && tasks()[idx].present
}

/// True when the task is registered and its sleep deadline is still in the
/// future (diagnostics/TUI display).
pub fn task_asleep(pid: u32) -> bool {
    let idx = pid as usize;
    if idx == 0 || idx >= MAX_TASKS {
        return false;
    }
    let t = tasks();
    t[idx].present && t[idx].sleep_until != 0 && t[idx].sleep_until > TICKS.load(Ordering::Relaxed)
}

/// Called from the timer interrupt path; performs the periodic preemptive
/// switch by rewriting the trap frame in place.
pub fn tick(frame: &mut InterruptFrame) {
    let ticks = TICKS.load(Ordering::Relaxed);
    unsafe {
        if ticks - LAST_SWITCH_TICK < SWITCH_DIVIDER {
            return;
        }
        LAST_SWITCH_TICK = ticks;
    }
    schedule(frame);
}

pub fn schedule(frame: &mut InterruptFrame) {
    let ticks = TICKS.load(Ordering::Relaxed);
    let t = tasks();
    let cur = unsafe {
        #[allow(static_mut_refs)]
        CURRENT
    };
    unsafe {
        #[allow(static_mut_refs)]
        if cur == 0 && BOOT_ACTIVE {
            return;
        }
    }

    // A hardware IRQ (PIT) never switches a running ring-0 kernel task away:
    // its resume point would land inside transient print/formatting code whose
    // live stack pointer differs from the frame the task's code was compiled
    // against (idle_loop hoists an argument anchor at entry). Ring-0 threads
    // switch only through `yield_kernel` (`int 0xfa`), so every resume happens
    // at a known, rsp-stable instruction. The boot hand-off (CUR == -1) is the
    // exception: its frame is abandoned by `boot_finished`, never resumed, so
    // it may be replaced by the fabricated idle frame.
    if (32..=47).contains(&frame.vector) && frame.cs == KERNEL_CS as u64 && cur != -1 {
        return;
    }

    if cur >= 0 {
        // In 64-bit mode the CPU always pushes SS/RSP on interrupt entry,
        // even when the interrupt stays in ring 0, so `frame.rsp` holds the
        // real pre-interrupt stack pointer for both ring-0 and ring-3
        // captures. The frame base sits 16 bytes below that (offsets +168..+184).
        let true_rsp = frame.rsp;
        if !t[cur as usize].valid_frame {
            t[cur as usize].frame = *frame;
            t[cur as usize].frame.rsp = true_rsp;
            t[cur as usize].valid_frame = true;
        } else if t[cur as usize].present {
            // Refresh only CPU-owned fields so fabricated user frames keep their
            // segment registers while general registers track live progress.
            let saved = &mut t[cur as usize].frame;
            saved.r8 = frame.r8;
            saved.r9 = frame.r9;
            saved.r10 = frame.r10;
            saved.r11 = frame.r11;
            saved.r12 = frame.r12;
            saved.r13 = frame.r13;
            saved.r14 = frame.r14;
            saved.r15 = frame.r15;
            saved.rax = frame.rax;
            saved.rcx = frame.rcx;
            saved.rdx = frame.rdx;
            saved.rbx = frame.rbx;
            saved.rbp = frame.rbp;
            saved.rsi = frame.rsi;
            saved.rdi = frame.rdi;
            saved.rip = frame.rip;
            saved.rsp = true_rsp;
            saved.cs = frame.cs;
            saved.ss = frame.ss;
            saved.rflags = frame.rflags;
        }
    }

    // Pick the next runnable slot; sleeping tasks are skipped until their
    // deadline passes, then woken in place. With every other task asleep the
    // rotation lands on slot 0 (the boot/idle context).
    let mut next: isize = -1;
    for step in 1..=MAX_TASKS as isize {
        let cand = ((cur + step) % MAX_TASKS as isize) as usize;
        if !t[cand].present || cand as isize == cur {
            continue;
        }
        let deadline = t[cand].sleep_until;
        if deadline != 0 {
            if deadline <= ticks {
                t[cand].sleep_until = 0;
                vprintln!("[sched] pid {} woke", cand);
                kprintln!("[serial] [sched] pid {} woke", cand);
            } else {
                continue;
            }
        }
        next = cand as isize;
        break;
    }
    if next < 0 || next == cur {
        return;
    }

    if next == 0 {
        // Rate-limit to one line per second: on a dead-PIC board the idle
        // task synthesizes a tick every ~10 ms and would spam the console at
        // ~100 lines/s while tasks sleep.
        let do_print = unsafe {
            #[allow(static_mut_refs)]
            {
                static mut IDLE_PRINTED: bool = false;
                static mut LAST_IDLE_PRINT: u64 = 0;
                let p = !IDLE_PRINTED || ticks.wrapping_sub(LAST_IDLE_PRINT) >= TIMER_HZ;
                if p {
                    IDLE_PRINTED = true;
                    LAST_IDLE_PRINT = ticks;
                }
                p
            }
        };
        if do_print {
            vprintln!("[sched] idle");
            kprintln!("[serial] [sched] idle");
        }
    }

    let incoming = t[next as usize].frame;
    unsafe {
        #[allow(static_mut_refs)]
        {
            CURRENT = next;
            SWITCH_COUNT += 1;
        }
    }
    if incoming.cs == KERNEL_CS as u64 {
        unsafe {
            aios_restore_ring0(core::ptr::addr_of!(incoming));
        }
    } else {
        unsafe {
            aios_restore_ring3(core::ptr::addr_of!(incoming));
        }
    }
}

/// Puts the current task to sleep for `ticks` PIT ticks and yields the CPU.
///
/// Called from the `SYS_SLEEP` syscall path (interrupts disabled, so the
/// global task table is safe to touch). The caller must have set the success
/// status in `frame.rax` beforehand — it is preserved into the task's saved
/// frame by the outgoing-context refresh above. A zero or negative request
/// still sleeps for at least one tick so the task always gets rescheduled.
pub fn sleep_current(frame: &mut InterruptFrame, ticks: u64) {
    let now = TICKS.load(Ordering::Relaxed);
    let t = tasks();
    let cur = unsafe {
        #[allow(static_mut_refs)]
        CURRENT
    };
    if cur > 0 && t[cur as usize].present {
        t[cur as usize].sleep_until = now.saturating_add(ticks.max(1));
    }
    schedule(frame);
}

/// Cooperative switch point for ring-0 kernel threads.
///
/// Executes `int 0xfa` (interrupt gate 250, DPL 0): the CPU pushes a trap
/// frame on the caller's own kernel stack exactly like the timer path, so the
/// scheduler captures the live stack pointer (`frame.rsp`) and later resumes
/// the task at the instruction right after this call. Ring-0 tasks are never
/// switched by a hardware IRQ; they switch only here, at a rsp-stable point.
pub fn yield_kernel() {
    unsafe {
        core::arch::asm!("int 0xfa", options(nostack));
    }
}
