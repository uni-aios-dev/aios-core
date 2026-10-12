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
//!
//! Fault recovery (v2.46.0) adds two more gates: `SYS_PING`, the cooperative
//! progress heartbeat that keeps the scheduler's hang watchdog from false-
//! flagging a long compute loop, and `SYS_FAULT_POLL`, which drains the bounded
//! kernel fault event ring so a ring-3 monitor can observe the kernel's
//! recovery traffic (the freestanding twin of the host `FaultReport` IPC).
//!
//! Networking (v2.49.0) adds the `SYS_SOCK_*` family — open/bind/connect/send/
//! recv/close/status over the kernel IPv4 socket layer [`crate::sock`], with
//! `listen`/`accept` (server side) since v2.50.0. Every buffer argument is a
//! user virtual address copied byte-by-byte through [`crate::memory::translate`],
//! and each operation is gated on socket ownership (`pid`) so one task cannot
//! touch another's descriptors.

use alloc::string::String;
use core::ptr::addr_of_mut;

use crate::{kprintln, sock, vprintln};

pub const SYS_SEND: u64 = 1;
pub const SYS_RECV: u64 = 2;
pub const SYS_WRITE: u64 = 3;
pub const SYS_GETPID: u64 = 4;
pub const SYS_SLEEP: u64 = 5;
pub const SYS_GUI: u64 = 6;
/// Progress heartbeat (v2.46.0): a ring-3 task that computes for a long time
/// without issuing other syscalls calls `SYS_PING` to tell the hang watchdog it
/// is alive. Bumps the task's activity clock and returns `0` in `rax`.
pub const SYS_PING: u64 = 7;
/// Drain one kernel fault event — the kernel→ring-3 recovery feed (the
/// freestanding twin of the host `aios_core::fault::FaultReport`). Returns
/// `rax = pid` (or `u64::MAX` when the ring is empty), `rcx = faulting rip`,
/// and `rdx = kind | (count << 32)`.
pub const SYS_FAULT_POLL: u64 = 8;

// --- IPv4 socket layer (v2.49.0) ------------------------------------------
/// Open a socket: `rdi = kind` (0 UDP, 1 TCP). Returns `rax = fd`, or
/// `u64::MAX` when the table is full.
pub const SYS_SOCKET: u64 = 9;
/// Bind a fixed local port: `rdi = fd`, `rsi = port`. Returns `rax = 1|0`.
pub const SYS_SOCK_BIND: u64 = 10;
/// Set the peer / start a TCP connect: `rdi = fd`, `rsi = IPv4` packed
/// big-endian in the low 32 bits (`a<<24|b<<16|c<<8|d`), `rdx = port`.
/// Returns `rax = 1|0`.
pub const SYS_SOCK_CONNECT: u64 = 11;
/// Send to the connected peer: `rdi = fd`, `rsi = user ptr`, `rdx = len`.
/// Returns `rax = bytes accepted`, or `u64::MAX` when busy/unconnected.
pub const SYS_SOCK_SEND: u64 = 12;
/// Receive: `rdi = fd`, `rsi = user ptr`, `rdx = len`. Returns `rax = bytes`
/// (`> 0`), `0` at end-of-stream (TCP FIN), or `u64::MAX` when nothing is
/// available yet.
pub const SYS_SOCK_RECV: u64 = 13;
/// Close a socket: `rdi = fd`. TCP sends a FIN; a second call frees the slot.
pub const SYS_SOCK_CLOSE: u64 = 14;
/// Lifecycle code of `fd` (1 connecting, 2 open, 3 closing, 4 closed,
/// 5 time-wait, 6 listening): `rdi = fd`. Returns `rax = code`.
pub const SYS_SOCK_STATUS: u64 = 15;
/// Start listening on a bound TCP socket: `rdi = fd`. Returns `rax = 1|0`.
pub const SYS_SOCK_LISTEN: u64 = 16;
/// Hand out the next established connection of the listener at `rdi = fd`.
/// Returns `rax = child fd`, or `u64::MAX` when none has completed yet.
pub const SYS_SOCK_ACCEPT: u64 = 17;

/// Scratch buffer for `SYS_SOCK_SEND`/`SYS_SOCK_RECV` user copies.
const SOCK_IO_CAP: usize = 1600;

/// Maximum number of user string bytes a single `SYS_WRITE` copies into the
/// kernel console (a hard cap against runaway pointers).
const SYS_STR_CAP: usize = 64;

/// `SYS_WRITE` calls are logged every `WRITE_LOG_EVERY`-th invocation so a
/// busy producer cannot flood the console proof lines.
const WRITE_LOG_EVERY: u64 = 32;

/// Highest task id reachable through the syscall gate.
pub const MAX_PID: usize = 6;
const MAILBOX_DEPTH: usize = 16;

/// `SYS_SEND`/`SYS_RECV` console+serial lines are emitted every
/// `IPC_LOG_EVERY`-th transferred value so the live ping-pong between the
/// ring-3 demo tasks is visible on hardware without flooding the console.
const IPC_LOG_EVERY: u64 = 8;

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

/// Occupancy of `pid`'s mailbox (0..=MAILBOX_DEPTH, diagnostics/TUI display).
pub fn mailbox_len(pid: u32) -> usize {
    let d = pid as usize;
    if d == 0 || d >= MAX_PID {
        return 0;
    }
    mailbox(d).len
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
    // Every syscall is forward progress: tell the hang watchdog so a task
    // looping on one `int 0x80` site is never declared hung.
    crate::sched::note_activity(pid);
    match frame.rax {
        SYS_SEND => {
            let ok = send(pid, frame.rdi as u32, frame.rcx);
            if ok && frame.rcx.is_multiple_of(IPC_LOG_EVERY) {
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
                if value % IPC_LOG_EVERY == 0 {
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
        SYS_GUI => {
            crate::gui::client_syscall(pid, frame);
        }
        SYS_PING => {
            // note_activity already ran at entry; the pings are intentionally
            // silent so a long compute loop can heartbeat without console spam.
            frame.rax = 0;
        }
        SYS_FAULT_POLL => match crate::fault::poll_event() {
            Some(event) => {
                frame.rax = event.pid as u64;
                frame.rcx = event.rip;
                frame.rdx = event.kind as u64 | ((event.count as u64) << 32);
            }
            None => {
                frame.rax = u64::MAX;
            }
        },
        SYS_SOCKET => {
            let kind = if frame.rdi == 1 {
                sock::Kind::Tcp
            } else {
                sock::Kind::Udp
            };
            match sock::open(pid, kind) {
                Some(fd) => {
                    vprintln!(
                        "[sock] pid {} open {} fd {}",
                        pid,
                        if frame.rdi == 1 { "tcp" } else { "udp" },
                        fd
                    );
                    kprintln!(
                        "[serial] [sock] pid {} open {} fd {}",
                        pid,
                        if frame.rdi == 1 { "tcp" } else { "udp" },
                        fd
                    );
                    frame.rax = fd as u64;
                }
                None => frame.rax = u64::MAX,
            }
        }
        SYS_SOCK_BIND => {
            let fd = frame.rdi as usize;
            let ok = sock_owned(pid, fd) && sock::bind(fd, frame.rsi as u16);
            frame.rax = ok as u64;
        }
        SYS_SOCK_CONNECT => {
            let fd = frame.rdi as usize;
            let ip = (frame.rsi as u32).to_be_bytes();
            let port = frame.rdx as u16;
            let ok = sock_owned(pid, fd) && sock::connect(fd, ip, port);
            if ok {
                vprintln!(
                    "[sock] fd {} connect {}.{}.{}.{}:{}",
                    fd,
                    ip[0],
                    ip[1],
                    ip[2],
                    ip[3],
                    port
                );
                kprintln!(
                    "[serial] [sock] fd {} connect {}.{}.{}.{}:{}",
                    fd,
                    ip[0],
                    ip[1],
                    ip[2],
                    ip[3],
                    port
                );
            }
            frame.rax = ok as u64;
        }
        SYS_SOCK_SEND => {
            let fd = frame.rdi as usize;
            if !sock_owned(pid, fd) {
                frame.rax = u64::MAX;
            } else {
                let cap = (frame.rdx as usize).min(SOCK_IO_CAP);
                let buf = unsafe { &mut *addr_of_mut!(SOCK_IO) };
                let n = user_read(frame.rsi, &mut buf[..cap]);
                match sock::send(fd, &buf[..n]) {
                    Some(w) => {
                        vprintln!("[sock] fd {} send {} bytes", fd, w);
                        kprintln!("[serial] [sock] fd {} send {} bytes", fd, w);
                        frame.rax = w as u64;
                    }
                    None => frame.rax = u64::MAX,
                }
            }
        }
        SYS_SOCK_RECV => {
            let fd = frame.rdi as usize;
            if !sock_owned(pid, fd) {
                frame.rax = u64::MAX;
            } else {
                let cap = (frame.rdx as usize).min(SOCK_IO_CAP);
                let buf = unsafe { &mut *addr_of_mut!(SOCK_IO) };
                match sock::recv(fd, &mut buf[..cap]) {
                    Some(0) => frame.rax = 0,
                    Some(n) => {
                        let w = user_write(frame.rsi, &buf[..n]);
                        vprintln!("[sock] fd {} recv {} bytes", fd, w);
                        kprintln!("[serial] [sock] fd {} recv {} bytes", fd, w);
                        frame.rax = w as u64;
                    }
                    None => frame.rax = u64::MAX,
                }
            }
        }
        SYS_SOCK_CLOSE => {
            let fd = frame.rdi as usize;
            if sock_owned(pid, fd) {
                sock::close(fd);
            }
            frame.rax = 0;
        }
        SYS_SOCK_STATUS => {
            frame.rax = sock::status(frame.rdi as usize) as u64;
        }
        SYS_SOCK_LISTEN => {
            let fd = frame.rdi as usize;
            let ok = sock_owned(pid, fd) && sock::listen(fd);
            frame.rax = ok as u64;
        }
        SYS_SOCK_ACCEPT => {
            let fd = frame.rdi as usize;
            if !sock_owned(pid, fd) {
                frame.rax = u64::MAX;
            } else {
                match sock::accept(fd) {
                    Some(cfd) => {
                        vprintln!("[sock] fd {} accept -> {}", fd, cfd);
                        kprintln!("[serial] [sock] fd {} accept -> {}", fd, cfd);
                        frame.rax = cfd as u64;
                    }
                    None => frame.rax = u64::MAX,
                }
            }
        }
        other => {
            vprintln!("[sysc] unknown syscall {} from pid {}", other, pid);
            kprintln!("[serial] [sysc] unknown syscall {}", other);
            frame.rax = u64::MAX;
        }
    }
}

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

/// Scratch buffer reused by `SYS_SOCK_SEND`/`SYS_SOCK_RECV` user copies.
static mut SOCK_IO: [u8; SOCK_IO_CAP] = [0; SOCK_IO_CAP];

/// `true` when `fd` is a live socket owned by `pid` (guards every socket op).
fn sock_owned(pid: u32, fd: usize) -> bool {
    sock::owner(fd) == pid && pid != 0
}

/// Copies up to `out.len()` bytes from the user pointer `ptr` into `out`,
/// validating every byte's virtual address so a bogus pointer cannot fault the
/// kernel. Returns the number of bytes actually read.
fn user_read(ptr: u64, out: &mut [u8]) -> usize {
    let mut n = 0;
    while n < out.len() {
        if crate::memory::translate(ptr + n as u64).is_none() {
            break;
        }
        out[n] = unsafe { core::ptr::read_volatile((ptr + n as u64) as *const u8) };
        n += 1;
    }
    n
}

/// Copies `src` to the user pointer `ptr`, validating every byte's virtual
/// address. Returns the number of bytes actually written.
fn user_write(ptr: u64, src: &[u8]) -> usize {
    let mut n = 0;
    while n < src.len() {
        if crate::memory::translate(ptr + n as u64).is_none() {
            break;
        }
        unsafe { core::ptr::write_volatile((ptr + n as u64) as *mut u8, src[n]) };
        n += 1;
    }
    n
}

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
        crate::console::write_bytes(&buf[..n]);
    }
    n
}
