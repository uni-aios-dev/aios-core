//! Preemptive round-robin scheduler (Milestone 3).
//!
//! Tasks are full trap-frame snapshots ([`InterruptFrame`]); switching means
//! saving the frame captured by the timer ISR into the outgoing task and
//! overwriting the trap frame on the stack with the incoming task's saved
//! state, so the final `iretq` resumes (or enters) the chosen task — ring 0
//! tasks resume inside their interrupted code, ring 3 tasks are entered
//! through a fabricated user-mode frame (CS=0x1B / SS=0x23).

use crate::interrupts::{TIMER_HZ, TICKS};
use crate::interrupts::InterruptFrame;
use crate::gdt::{USER_CS, USER_DS};
use crate::{kprintln, vprintln};
use core::sync::atomic::Ordering;

/// Switch cadence: preemption fires 4 times per second.
const SWITCH_DIVIDER: u64 = TIMER_HZ / 4;

pub const MAX_TASKS: usize = 4;

#[derive(Clone, Copy)]
struct Task {
    present: bool,
    /// Set once the task's frame holds real state (boot task captures its
    /// frame on the first switch away; spawned frames start fabricated).
    valid_frame: bool,
    frame: InterruptFrame,
}

impl Task {
    const fn empty() -> Self {
        Self {
            present: false,
            valid_frame: false,
            frame: InterruptFrame {
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
];

/// Index of the currently scheduled task (`0` = boot/idle context).
static mut CURRENT: isize = -1;
static mut LAST_SWITCH_TICK: u64 = 0;
static mut SWITCH_COUNT: u64 = 0;

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
    vprintln!("[sched] ready (idle=task0, round-robin every {} ticks)", SWITCH_DIVIDER);
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
    for slot in 1..MAX_TASKS {
        if !t[slot].present {
            fill(&mut t[slot].frame);
            t[slot].present = true;
            t[slot].valid_frame = true;
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

fn schedule(frame: &mut InterruptFrame) {
    let t = tasks();
    let cur = unsafe {
        #[allow(static_mut_refs)]
        CURRENT
    };
    if cur < 0 {
        return;
    }

    // Capture the outgoing context once.
    if !t[cur as usize].valid_frame {
        t[cur as usize].frame = *frame;
        t[cur as usize].valid_frame = true;
    } else if t[cur as usize].present {
        // Refresh only CPU-owned fields so fabricated user frames keep their
        // segment registers while general registers track live progress.
        let saved = &mut t[cur as usize].frame;
        saved.rax = frame.rax;
        saved.rcx = frame.rcx;
        saved.rdx = frame.rdx;
        saved.rbx = frame.rbx;
        saved.rsi = frame.rsi;
        saved.rdi = frame.rdi;
        saved.rip = frame.rip;
        saved.rsp = frame.rsp;
        saved.cs = frame.cs;
        saved.ss = frame.ss;
        saved.rflags = frame.rflags;
    }

    // Pick the next runnable slot.
    let mut next: isize = -1;
    for step in 1..=MAX_TASKS as isize {
        let cand = ((cur + step) % MAX_TASKS as isize) as usize;
        if t[cand].present && cand as isize != cur {
            next = cand as isize;
            break;
        }
    }
    if next < 0 || next == cur {
        return;
    }

    let incoming = t[next as usize].frame;
    *frame = incoming;
    unsafe {
        #[allow(static_mut_refs)]
        {
            CURRENT = next;
            SWITCH_COUNT += 1;
        }
    }
}
