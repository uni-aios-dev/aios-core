//! Kernel-side system calls behind the `int 0x80` gate (Milestones 4+5).
//!
//! The `int 0x80` dispatcher owns every privilege transition a ring-3 program
//! can make:
//!
//! - Milestone 4: IPC mailboxes (`SYS_SEND`/`SYS_RECV`) that mirror the packet
//!   semantics of the userspace `aios_core::ipc_protocol::IpcPacket` — every
//!   message carries an [`IpcPacketHeader`] (`src`/`dst`/`kind`/`len`) plus a
//!   payload word; one bounded mailbox per task id.
//! - Milestone 5: console I/O (`SYS_WRITE`), the process id query
//!   (`SYS_GETPID`) and tick-based blocking sleep (`SYS_SLEEP`), which
//!   cooperates with the scheduler so a sleeping task stops consuming CPU and
//!   the kernel falls back to the idle context until the deadline passes.

use alloc::string::String;

use crate::{kprintln, vprintln};

pub const SYS_SEND: u64 = 1;
pub const SYS_RECV: u64 = 2;
pub const SYS_WRITE: u64 = 3;
pub const SYS_GETPID: u64 = 4;
pub const SYS_SLEEP: u64 = 5;

/// Maximum number of user string bytes a single `SYS_WRITE` copies into the
/// kernel console (a hard cap against runaway pointers).
const SYS_STR_CAP: usize = 64;

/// `SYS_WRITE` calls are logged every `WRITE_LOG_EVERY`-th invocation so a
/// busy producer cannot flood the console proof lines.
const WRITE_LOG_EVERY: u64 = 32;

/// Highest task id reachable through the syscall gate.
pub const MAX_PID: usize = 5;
const MAILBOX_DEPTH: usize = 16;

/// Wire header mirrored from `aios_core::ipc_protocol` (fixed layout so the
/// concept survives into the freestanding kernel). The kernel-side mailbox
/// stores packets in unpacked form; the header documents the on-wire shape
/// shared with the userspace protocol.
#[repr(C)]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct IpcPacketHeader {
    pub src: u32,
    pub dst: u32,
    pub kind: u32,
    pub len: u32,
}

struct Mailbox {
    buf: [u64; MAILBOX_DEPTH],
    srcs: [u32; MAILBOX_DEPTH],
    head: usize,
    len: usize,
}

impl Mailbox {
    const fn new() -> Self {
        Self {
            buf: [0; MAILBOX_DEPTH],
            srcs: [0; MAILBOX_DEPTH],
            head: 0,
            len: 0,
        }
    }
}

static mut MAILBOXES: [Mailbox; MAX_PID] = [
    Mailbox::new(),
    Mailbox::new(),
    Mailbox::new(),
    Mailbox::new(),
    Mailbox::new(),
];

static mut SENT_TOTAL: u64 = 0;
static mut RECV_TOTAL: u64 = 0;

fn mailbox(pid: usize) -> &'static mut Mailbox {
    unsafe {
        #[allow(static_mut_refs)]
        &mut *core::ptr::addr_of_mut!(MAILBOXES[pid])
    }
}

/// Enqueue `value` from `src` into `dst`'s mailbox.
///
/// Returns false when `dst` is out of range or its mailbox is full (the
/// packet is dropped, matching lossy best-effort semantics documented for
/// the kernel rings).
pub fn send(src: u32, dst: u32, value: u64) -> bool {
    let d = dst as usize;
    if d == 0 || d >= MAX_PID || src == dst {
        return false;
    }
    let mb = mailbox(d);
    unsafe {
        if mb.len >= MAILBOX_DEPTH {
            return false;
        }
        let tail = (mb.head + mb.len) % MAILBOX_DEPTH;
        mb.buf[tail] = value;
        mb.srcs[tail] = src;
        mb.len += 1;
        SENT_TOTAL += 1;
    }
    true
}

/// Dequeue the oldest packet addressed to `dst`.
pub fn recv(dst: u32) -> Option<(u32, u64)> {
    let d = dst as usize;
    if d == 0 || d >= MAX_PID {
        return None;
    }
    let mb = mailbox(d);
    unsafe {
        if mb.len == 0 {
            return None;
        }
        let value = mb.buf[mb.head];
        let src = mb.srcs[mb.head];
        mb.head = (mb.head + 1) % MAILBOX_DEPTH;
        mb.len -= 1;
        RECV_TOTAL += 1;
        Some((src, value))
    }
}

pub fn stats() -> (u64, u64) {
    unsafe {
        #[allow(static_mut_refs)]
        (SENT_TOTAL, RECV_TOTAL)
    }
}

/// `int 0x80` dispatcher invoked from the interrupt path.
///
/// Contract: `eax` selects the syscall. Arguments: `edi` = destination pid for
/// `SYS_SEND` / ticks for `SYS_SLEEP`; `ecx` = payload word for `SYS_SEND`;
/// `esi` = NUL-terminated string pointer for `SYS_WRITE`. Return values land
/// in `rax` (send/receive ok flag, source pid or `-1` for an empty receive,
/// pid for `SYS_GETPID`, bytes written or `0` for the others) and `rcx` holds
/// a received payload word.
pub fn syscall(frame: &mut crate::interrupts::InterruptFrame) {
    let pid = crate::sched::current_pid();
    note_first(pid);
    match frame.rax {
        SYS_SEND => {
            let ok = send(pid, frame.rdi as u32, frame.rcx);
            if ok && frame.rcx.is_multiple_of(SAMPLE_EVERY) {
                vprintln!(
                    "[ipc] send pid{} -> pid{} val={}",
                    pid,
                    frame.rdi as u32,
                    frame.rcx
                );
                kprintln!(
                    "[serial] [ipc] send {}->{} val={}",
                    pid,
                    frame.rdi as u32,
                    frame.rcx
                );
            }
            frame.rax = ok as u64;
        }
        SYS_RECV => match recv(pid) {
            Some((src, value)) => {
                if value % SAMPLE_EVERY == 0 {
                    vprintln!("[ipc] recv pid{} <- pid{} val={}", pid, src, value);
                    kprintln!("[serial] [ipc] recv {}<-{} val={}", pid, src, value);
                }
                frame.rax = src as u64;
                frame.rcx = value;
            }
            None => {
                frame.rax = u64::MAX;
            }
        },
        SYS_WRITE => {
            let n = write_user_string(frame);
            frame.rax = n as u64;
            let calls = write_calls(pid);
            if calls % WRITE_LOG_EVERY == 1 && n > 0 {
                let slice = unsafe {
                    core::slice::from_raw_parts(core::ptr::addr_of!(USER_STR_BUF) as *const u8, n)
                };
                let shown = String::from_utf8_lossy(slice);
                vprintln!("[sysc] pid {} write {}: {}", pid, n, shown);
                kprintln!("[serial] [sysc] pid {} write {}: {}", pid, n, shown);
            }
        }
        SYS_GETPID => {
            frame.rax = pid as u64;
            note_getpid(pid);
        }
        SYS_SLEEP => {
            let ticks = frame.rdi;
            vprintln!("[sched] pid {} sleep {} ticks", pid, ticks);
            kprintln!("[serial] [sched] pid {} sleep {}", pid, ticks);
            frame.rax = 0;
            crate::sched::sleep_current(frame, ticks);
        }
        other => {
            vprintln!("[sysc] unknown syscall {} from pid {}", other, pid);
            kprintln!("[serial] [sysc] unknown syscall {}", other);
            frame.rax = u64::MAX;
        }
    }
}

const SAMPLE_EVERY: u64 = 256;

static mut FIRST_SYSCALL_SEEN: [bool; MAX_PID] = [false; MAX_PID];

fn note_first(pid: u32) {
    let idx = pid as usize;
    if idx == 0 || idx >= MAX_PID {
        return;
    }
    unsafe {
        let seen = &mut *core::ptr::addr_of_mut!(FIRST_SYSCALL_SEEN);
        if !seen[idx] {
            seen[idx] = true;
            vprintln!("[ring3] first syscall from pid {} (DPL gate OK)", pid);
            kprintln!("[serial] [ring3] pid {} entered ring 3 via int 0x80", pid);
        }
    }
}

static mut GETPID_SEEN: [bool; MAX_PID] = [false; MAX_PID];

fn note_getpid(pid: u32) {
    let idx = pid as usize;
    if idx == 0 || idx >= MAX_PID {
        return;
    }
    unsafe {
        let seen = &mut *core::ptr::addr_of_mut!(GETPID_SEEN);
        if !seen[idx] {
            seen[idx] = true;
            vprintln!("[sysc] pid {} getpid -> {}", pid, pid);
            kprintln!("[serial] [sysc] pid {} getpid -> {}", pid, pid);
        }
    }
}

/// Per-pid count of `SYS_WRITE` calls so the sampled proof lines land on the
/// first write of every block regardless of its byte length.
static mut WRITE_CALLS: [u64; MAX_PID] = [0; MAX_PID];

fn write_calls(pid: u32) -> u64 {
    let idx = pid as usize;
    if idx == 0 || idx >= MAX_PID {
        return 0;
    }
    unsafe {
        let calls = &mut *core::ptr::addr_of_mut!(WRITE_CALLS);
        calls[idx] += 1;
        calls[idx]
    }
}

/// Scratch buffer reused by `SYS_WRITE` (never read past `SYS_STR_CAP`).
static mut USER_STR_BUF: [u8; SYS_STR_CAP] = [0; SYS_STR_CAP];

/// Copies the user string at `frame.rsi` into the kernel console.
///
/// Reads byte-by-byte, validating each byte's virtual address through the
/// page tables so a bogus pointer can never fault the kernel; opaque bytes
/// (control chars) are dropped. Logs the first write of every 32-write block
/// from `syscall`, while the terminal still receives every valid byte.
fn write_user_string(frame: &crate::interrupts::InterruptFrame) -> usize {
    let mut ptr = frame.rsi;
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(USER_STR_BUF) };
    let mut n: usize = 0;
    loop {
        if n == SYS_STR_CAP {
            break;
        }
        if crate::memory::translate(ptr).is_none() {
            break;
        }
        let byte = unsafe { core::ptr::read_volatile(ptr as *const u8) };
        ptr += 1;
        if byte == 0 {
            break;
        }
        if byte == b'\n' || byte == b'\t' || (0x20..=0x7E).contains(&byte) {
            buf[n] = byte;
            n += 1;
        }
    }
    if n > 0 {
        crate::serial::write_bytes(&buf[..n]);
        crate::vga::write_bytes(&buf[..n]);
    }
    n
}
