//! Kernel-side IPC mailboxes (Milestone 4).
//!
//! Mirrors the packet semantics of the userspace
//! `aios_core::ipc_protocol::IpcPacket`: every message carries a
//! [`IpcPacketHeader`] (`src`/`dst`/`kind`/`len`) plus a payload word. The
//! no_std kernel keeps one bounded mailbox per task id; ring-3 programs
//! reach it through the `int 0x80` syscalls `SYS_SEND` / `SYS_RECV`.

use crate::{kprintln, vprintln};

pub const SYS_SEND: u64 = 1;
pub const SYS_RECV: u64 = 2;

/// Highest task id reachable through the syscall gate.
pub const MAX_PID: usize = 4;
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
        if (*mb).len >= MAILBOX_DEPTH {
            return false;
        }
        let tail = ((*mb).head + (*mb).len) % MAILBOX_DEPTH;
        (*mb).buf[tail] = value;
        (*mb).srcs[tail] = src;
        (*mb).len += 1;
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
        if (*mb).len == 0 {
            return None;
        }
        let value = (*mb).buf[(*mb).head];
        let src = (*mb).srcs[(*mb).head];
        (*mb).head = ((*mb).head + 1) % MAILBOX_DEPTH;
        (*mb).len -= 1;
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

/// `int 0x80` dispatcher invoked from the interrupt path.///
/// Contract: `eax` selects the syscall, `edi` the destination pid (send)
/// and `ecx` carries the payload word. Return values land in `rax`
/// (source pid or `-1` for an empty receive) and `rcx` (received value).
pub fn syscall(frame: &mut crate::interrupts::InterruptFrame) {
    let pid = crate::sched::current_pid();
    match frame.rax {
        SYS_SEND => {
            note_first(pid);
            let ok = send(pid, frame.rdi as u32, frame.rcx);
            if ok && frame.rcx % SAMPLE_EVERY == 0 {
                vprintln!("[ipc] send pid{} -> pid{} val={}", pid, frame.rdi as u32, frame.rcx);
                kprintln!("[serial] [ipc] send {}->{} val={}", pid, frame.rdi as u32, frame.rcx);
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
        other => {
            vprintln!("[ipc] unknown syscall {} from pid {}", other, pid);
            kprintln!("[serial] [ipc] unknown syscall {}", other);
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

