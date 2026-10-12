//! Ring-3 demo programs and their memory setup (Milestones 3-5, sockets v2.51.0).
//!
//! Five tiny user programs are copied into freshly mapped user pages and
//! entered through the scheduler's fabricated ring-3 frames:
//!
//! - program A (slot 2): sends its counter to slot 3, drains its inbox, then
//!   prints `u1` to the kernel console through `SYS_WRITE`;
//! - program B (slot 3): mirrors A with the roles swapped (`u2`);
//! - program C (slot 4): prints `[usleep] up` and its pid via `SYS_GETPID`,
//!   then alternates `SYS_SLEEP(20)` with a `*` echo forever;
//! - program D (slot 5): the ring-3 GUI client — paints a 96x64 pixel buffer
//!   with a rotating solid colour and presents it to the kernel window server
//!   via `SYS_GUI` every 6 ticks; each cycle it also drains one input event
//!   (`SYS_GUI` GET_EVENT) — a key turns the fill red, a body click turns it
//!   green, echoed back through `SYS_WRITE` (visible only while the GUI
//!   desktop owns the screen);
//! - program E (slot 6, v2.51.0): the ring-3 socket client — two UDP sockets
//!   bound to fixed local ports play a loopback ping-pong over the bound IP
//!   (`SYS_SOCK_ADDR` first), sending `ring3` from client to server and `ack!`
//!   back, printing `sock-e ok` once a round trip has completed. Since v2.52.0
//!   a second (TCP) leg follows: a TCP client connects to a listening socket,
//!   the accepted child echoes `tcp`/`ok` and is then closed, printing
//!   `sock-t ok`. It proves the *userspace* face of the socket layer: every
//!   `SYS_SOCK_*` call runs from ring 3 over `int 0x80` into the kernel `sock`
//!   table and the loopback ring.
//!
//! `SYS_SEND` target ids are *scheduler slot ids* (slots 2/3/4 — the kernel
//! worker owns slot 1); `SLOT_*` constants document the mapping. A and B form
//! a ping-pong over the per-pid mailboxes whose sampled `[ipc] send/recv`
//! proof lines appear every 8th transferred value.
//!
//! Every program starts by calling `SYS_SLEEP` (20 ticks) so the whole
//! userspace is briefly asleep at boot — the scheduler must fall back to the
//! idle context (`[sched] idle`) and wake the five as their deadlines pass,
//! proving sleep, wake and idle fallback in one QEMU run.

use alloc::vec::Vec;

use crate::memory;
use crate::sched;
use crate::syscalls::{
    SYS_GETPID, SYS_GUI, SYS_RECV, SYS_SEND, SYS_SLEEP, SYS_SOCKET, SYS_SOCK_ACCEPT, SYS_SOCK_ADDR,
    SYS_SOCK_BIND, SYS_SOCK_CLOSE, SYS_SOCK_CONNECT, SYS_SOCK_LISTEN, SYS_SOCK_RECV, SYS_SOCK_SEND,
    SYS_WRITE,
};
use crate::{kprintln, vprintln};

pub const PID_A: u32 = 1;
pub const PID_B: u32 = 2;
pub const PID_C: u32 = 3;
pub const PID_D: u32 = 4;
pub const PID_E: u32 = 5;

/// Runtime slot ids seen by `current_pid()`/`SYS_*` calls. The kernel worker
/// takes slot 1, then the demo tasks are spawned A → B → C, so they land on
/// slots 2, 3, 4. `SYS_SEND` distances are the *slot* id of the target
/// mailbox, which is why the peer constants below differ from `PID_*`.
pub const SLOT_A: u32 = 2;
pub const SLOT_B: u32 = 3;

/// Per-task stride so code/stack regions never overlap.
const REGION_STRIDE: u64 = 0x20_0000; // 2 MiB per task
const CODE_BASE: u64 = 0x4000_0000;
const STACK_TOP: u64 = 0x7F00_0000;
const STACK_PAGES: u64 = 4;

/// Startup sleep in PIT ticks shared by all user programs so the whole
/// userspace idles together at boot.
const BOOT_SLEEP_TICKS: u64 = 20;
/// Sleep length the pid 3 sleeper repeats in its demo loop.
const SLEEPER_SLEEP_TICKS: u64 = 20;
/// Number of sleep/wake rounds the pid 3 sleeper performs.
const SLEEPER_ROUNDS: u64 = 8;
/// Ring-3 GUI client (pid 4) window: a 96x64 test surface, repainted with a
/// solid colour that rotates every 6 ticks.
const GUI_W: u64 = 96;
const GUI_H: u64 = 64;
const GUI_PIXELS: u64 = GUI_W * GUI_H;
const GUI_SLEEP_TICKS: u64 = 6;
/// Colour added to the client's fill each present round.
const GUI_COLOR_STEP: u64 = 0x00_11_2a_44;
/// Fill colour the demo switches to on a key event (red-ish).
const GUI_KEY_COLOR: u64 = 0x00_C0_40_40;
/// Fill colour the demo switches to on a click event (green-ish).
const GUI_CLICK_COLOR: u64 = 0x00_40_C0_40;

/// Ring-3 socket demo (pid 5): local port of the client socket.
const SOCK_CLI_PORT: u64 = 40_110;
/// Ring-3 socket demo (pid 5): local port of the server socket.
const SOCK_SRV_PORT: u64 = 40_111;
/// Ring-3 socket demo (pid 5): receive buffer length used by `SYS_SOCK_RECV`.
const SOCK_BUF_LEN: u64 = 16;
/// Ring-3 socket demo (pid 5): expected length of the inbound `ring3` datagram.
const SOCK_REQ_LEN: u64 = 5;
/// Ring-3 socket demo (pid 5): expected length of the inbound `ack!` datagram.
const SOCK_REPLY_LEN: u64 = 4;
/// Ring-3 socket demo (pid 5): local port of the TCP client socket.
const SOCK_TCP_CLI_PORT: u64 = 40_112;
/// Ring-3 socket demo (pid 5): local port of the TCP listener socket.
const SOCK_TCP_SRV_PORT: u64 = 40_113;
/// Ring-3 socket demo (pid 5): expected length of the inbound `tcp` segment.
const SOCK_TCP_MSG_LEN: u64 = 3;
/// Ring-3 socket demo (pid 5): expected length of the inbound `ok` segment.
const SOCK_TCP_REPLY_LEN: u64 = 2;

/// Tiny x86-64 emitter used to build the raw-machine-code demo programs.
///
/// String references are emitted as placeholders and patched in [`Asm::finish`]
/// once the trailing rodata section is laid out (their absolute virtual
/// address depends on the final program length).
struct Asm {
    buf: Vec<u8>,
    /// (byte offset of the imm32 field of a `mov esi` placeholder, string label)
    string_refs: Vec<(usize, usize)>,
    /// Buffer offsets of the appended strings, indexed by their label.
    strings: Vec<usize>,
}

impl Asm {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            string_refs: Vec::new(),
            strings: Vec::new(),
        }
    }

    fn mov_eax(&mut self, v: u64) {
        self.buf.push(0xB8);
        self.imm32(v);
    }

    fn mov_edi(&mut self, v: u64) {
        self.buf.push(0xBF);
        self.imm32(v);
    }

    /// `mov esi, imm32` with a placeholder patched later via [`Asm::finish`].
    fn mov_esi_placeholder(&mut self, label: usize) {
        self.buf.push(0xBE);
        self.string_refs.push((self.buf.len(), label));
        self.imm32(0);
    }

    fn xor_edi(&mut self) {
        self.buf.extend_from_slice(&[0x31, 0xFF]);
    }

    fn inc_ecx(&mut self) {
        self.buf.extend_from_slice(&[0xFF, 0xC1]);
    }

    fn int80(&mut self) {
        self.buf.extend_from_slice(&[0xCD, 0x80]);
    }

    fn mov_r8d(&mut self, v: u64) {
        self.buf.extend_from_slice(&[0x41, 0xB8]);
        self.imm32(v);
    }

    /// `mov eax, r8d` (44 89 C0) — a colour kept in a syscall-preserved register.
    fn mov_eax_r8d(&mut self) {
        self.buf.extend_from_slice(&[0x44, 0x89, 0xC0]);
    }

    /// `mov ecx, imm32` (loop counter for the buffer fill).
    fn mov_ecx_imm(&mut self, v: u64) {
        self.buf.push(0xB9);
        self.imm32(v);
    }

    /// `mov [edx], eax` with the 0x67 address-size override: 32-bit (zero-
    /// extended) addressing of the user pixel buffer from the current edx.
    fn store_edx_eax(&mut self) {
        self.buf.extend_from_slice(&[0x67, 0x89, 0x02]);
    }

    /// `add edx, imm8` walking the fill pointer by the pixel stride (4 bytes).
    fn add_edx_imm8(&mut self, v: u8) {
        self.buf.extend_from_slice(&[0x83, 0xC2, v]);
    }

    /// `dec ecx`.
    fn dec_ecx(&mut self) {
        self.buf.extend_from_slice(&[0xFF, 0xC9]);
    }

    /// `add r8d, imm32` rotating the fill colour between present rounds.
    fn add_r8d_imm32(&mut self, v: u64) {
        self.buf.extend_from_slice(&[0x41, 0x81, 0xC0]);
        self.imm32(v);
    }

    /// `test rax, rax` (48 85 C0) — a zero/non-zero test of a syscall result.
    fn test_rax_rax(&mut self) {
        self.buf.extend_from_slice(&[0x48, 0x85, 0xC0]);
    }

    /// `mov ecx, eax` (89 C1) — moves the event word into a scratch register.
    fn mov_ecx_eax(&mut self) {
        self.buf.extend_from_slice(&[0x89, 0xC1]);
    }

    /// `and ecx, 0xFF` (81 E1 imm32) — isolates the event type byte.
    fn and_ecx_ff(&mut self) {
        self.buf
            .extend_from_slice(&[0x81, 0xE1, 0xFF, 0x00, 0x00, 0x00]);
    }

    /// `cmp ecx, imm8` (83 F9 imm8) — compares the event type.
    fn cmp_ecx_imm8(&mut self, v: u8) {
        self.buf.extend_from_slice(&[0x83, 0xF9, v]);
    }

    /// `mov ebx, eax` (89 C3) — the socket demo keeps its IP snapshot in ebx,
    /// which the kernel gate preserves across `int 0x80`.
    fn mov_ebx_eax(&mut self) {
        self.buf.extend_from_slice(&[0x89, 0xC3]);
    }

    /// `mov esi, ebx` (89 DE) — the connect target IP into the syscall arg.
    fn mov_esi_ebx(&mut self) {
        self.buf.extend_from_slice(&[0x89, 0xDE]);
    }

    /// `mov edi, r8d` (44 89 C7) — the client fd into the syscall arg register.
    fn mov_edi_r8d(&mut self) {
        self.buf.extend_from_slice(&[0x44, 0x89, 0xC7]);
    }

    /// `mov edi, r9d` (44 89 CF) — the server fd into the syscall arg register.
    fn mov_edi_r9d(&mut self) {
        self.buf.extend_from_slice(&[0x44, 0x89, 0xCF]);
    }

    /// `mov r8d, eax` (41 89 C0) — persist the client fd across syscalls.
    fn mov_r8d_eax(&mut self) {
        self.buf.extend_from_slice(&[0x41, 0x89, 0xC0]);
    }

    /// `mov r9d, eax` (41 89 C1) — persist the server fd across syscalls.
    fn mov_r9d_eax(&mut self) {
        self.buf.extend_from_slice(&[0x41, 0x89, 0xC1]);
    }

    /// `mov r12d, eax` (41 89 C4) — persist the TCP client fd across syscalls.
    fn mov_r12d_eax(&mut self) {
        self.buf.extend_from_slice(&[0x41, 0x89, 0xC4]);
    }

    /// `mov r13d, eax` (41 89 C5) — persist the TCP listener fd across syscalls.
    fn mov_r13d_eax(&mut self) {
        self.buf.extend_from_slice(&[0x41, 0x89, 0xC5]);
    }

    /// `mov r14d, eax` (41 89 C6) — persist the accepted child fd.
    fn mov_r14d_eax(&mut self) {
        self.buf.extend_from_slice(&[0x41, 0x89, 0xC6]);
    }

    /// `mov edi, r12d` (44 89 E7) — the TCP client fd into the syscall arg.
    fn mov_edi_r12d(&mut self) {
        self.buf.extend_from_slice(&[0x44, 0x89, 0xE7]);
    }

    /// `mov edi, r13d` (44 89 EF) — the TCP listener fd into the syscall arg.
    fn mov_edi_r13d(&mut self) {
        self.buf.extend_from_slice(&[0x44, 0x89, 0xEF]);
    }

    /// `mov edi, r14d` (44 89 F7) — the accepted child fd into the syscall arg.
    fn mov_edi_r14d(&mut self) {
        self.buf.extend_from_slice(&[0x44, 0x89, 0xF7]);
    }

    /// `jne rel8` forward placeholder; patch with [`Self::patch_rel8`].
    fn jne_fwd(&mut self) -> usize {
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0x75, 0x00]);
        at
    }

    /// `cmp eax, imm8` (83 F8 imm8) — compares a `SYS_SOCK_RECV` length result.
    fn cmp_eax_imm8(&mut self, v: u8) {
        self.buf.extend_from_slice(&[0x83, 0xF8, v]);
    }

    /// `mov edx, imm32` (BA imm32) — a raw port/length literal into `rdx`.
    fn mov_edx_imm(&mut self, v: u64) {
        self.buf.push(0xBA);
        self.imm32(v);
    }

    /// `mov esi, imm32` (BE imm32) — a raw literal into `esi` (bind port).
    fn mov_esi_imm(&mut self, v: u64) {
        self.buf.push(0xBE);
        self.imm32(v);
    }

    /// `jz rel8` forward placeholder; patch with [`Self::patch_rel8`].
    fn jz_fwd(&mut self) -> usize {
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0x74, 0x00]);
        at
    }

    /// `je rel8` forward placeholder; patch with [`Self::patch_rel8`].
    fn je_fwd(&mut self) -> usize {
        self.jz_fwd()
    }

    /// `jmp rel8` forward placeholder; patch with [`Self::patch_rel8`].
    fn jmp_fwd(&mut self) -> usize {
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0xEB, 0x00]);
        at
    }

    /// Patches the rel8 displacement of the forward jump emitted at `at` to
    /// target the current end of the buffer.
    fn patch_rel8(&mut self, at: usize) {
        let rel = self.buf.len() as i64 - (at + 2) as i64;
        self.buf[at + 1] = rel as u8;
    }

    /// Appends a raw `u32` (part of a data section, e.g. the `SYS_GUI` request).
    fn data_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Patches the imm32 at byte position `pos` (earlier emitted as a
    /// placeholder) with an absolute virtual address once the layout is known.
    fn patch_imm32(&mut self, pos: usize, v: u32) {
        self.buf[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// `mov esi, imm32` with a manually-patched placeholder (returns the imm32
    /// byte position for [`Self::patch_imm32`]).
    fn mov_esi_abs(&mut self) -> usize {
        self.buf.push(0xBE);
        let pos = self.buf.len();
        self.imm32(0);
        pos
    }

    /// `mov edx, imm32` with a manually-patched placeholder (fill-buffer base).
    fn mov_edx_abs(&mut self) -> usize {
        self.buf.push(0xBA);
        let pos = self.buf.len();
        self.imm32(0);
        pos
    }

    fn dec_r8d(&mut self) {
        self.buf.extend_from_slice(&[0x41, 0xFF, 0xC8]);
    }

    /// `jnz rel8` jumping back to `back` (the position of the `dec r8d`).
    fn jnz_back(&mut self, back: usize) {
        let at = self.buf.len();
        let rel = back as i64 - (at + 2) as i64;
        self.buf.push(0x75);
        self.buf.push(rel as u8);
    }

    /// `jmp rel32` jumping back to `back` (the start of the loop).
    fn jmp_back(&mut self, back: usize) {
        let at = self.buf.len();
        let rel = back as i64 - (at + 5) as i64;
        self.buf.push(0xE9);
        self.buf.extend_from_slice(&(rel as u32).to_le_bytes());
    }

    /// Appends a NUL-terminated string, returns its offset in the buffer and
    /// records it so [`Asm::finish`] can resolve `mov esi` placeholders to it.
    fn string(&mut self, s: &[u8]) -> usize {
        let off = self.buf.len();
        self.buf.extend_from_slice(s);
        self.buf.push(0);
        self.strings.push(off);
        off
    }

    fn imm32(&mut self, v: u64) {
        self.buf.extend_from_slice(&(v as u32).to_le_bytes());
    }

    /// Patches the `mov esi` placeholder addresses and returns the program.
    fn finish(mut self, pid: u32) -> Vec<u8> {
        let base = CODE_BASE + pid as u64 * REGION_STRIDE;
        for (pos, label) in &self.string_refs {
            let off = self.strings[*label];
            let va = (base + off as u64) as u32;
            self.buf[*pos..*pos + 4].copy_from_slice(&va.to_le_bytes());
        }
        self.buf
    }
}

fn build_pid1() -> Vec<u8> {
    let mut a = Asm::new();
    let start = 0usize;

    a.mov_eax(SYS_SLEEP);
    a.mov_edi(BOOT_SLEEP_TICKS);
    a.int80();

    a.mov_eax(SYS_SEND);
    a.mov_edi(SLOT_B as u64);
    a.inc_ecx();
    a.int80();

    a.mov_eax(SYS_RECV);
    a.xor_edi();
    a.int80();

    a.mov_esi_placeholder(0);
    a.mov_eax(SYS_WRITE);
    a.int80();

    a.mov_r8d(0x0040_0000);
    let pacing = a.buf.len();
    a.dec_r8d();
    a.jnz_back(pacing);
    a.jmp_back(start);

    a.string(b"u1");
    a.finish(PID_A)
}

fn build_pid2() -> Vec<u8> {
    let mut a = Asm::new();
    let start = 0usize;

    a.mov_eax(SYS_SLEEP);
    a.mov_edi(BOOT_SLEEP_TICKS);
    a.int80();

    a.mov_eax(SYS_RECV);
    a.xor_edi();
    a.int80();

    a.mov_eax(SYS_SEND);
    a.mov_edi(SLOT_A as u64);
    a.inc_ecx();
    a.int80();

    a.mov_esi_placeholder(0);
    a.mov_eax(SYS_WRITE);
    a.int80();

    a.mov_r8d(0x0040_0000);
    let pacing = a.buf.len();
    a.dec_r8d();
    a.jnz_back(pacing);
    a.jmp_back(start);

    a.string(b"u2");
    a.finish(PID_B)
}

fn build_pid3() -> Vec<u8> {
    let mut a = Asm::new();

    a.mov_esi_placeholder(0);
    a.mov_eax(SYS_WRITE);
    a.int80();

    a.mov_eax(SYS_GETPID);
    a.int80();

    let cycle = a.buf.len();
    a.mov_r8d(SLEEPER_ROUNDS);
    let sleeper = a.buf.len();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(SLEEPER_SLEEP_TICKS);
    a.int80();

    a.mov_esi_placeholder(1);
    a.mov_eax(SYS_WRITE);
    a.int80();

    a.dec_r8d();
    a.jnz_back(sleeper);
    a.jmp_back(cycle);

    a.string(b"[usleep] up");
    a.string(b"*");
    a.finish(PID_C)
}

/// Ring-3 GUI client: fills its 96x64 pixel buffer with a solid colour and
/// presents it to the kernel window server every 6 ticks, rotating the colour
/// each round and draining one input event per cycle (`SYS_GUI` GET_EVENT: a
/// key recolours the fill red, a body click green, each echoed through
/// `SYS_WRITE`). CREATE runs once after the boot sleep; everything else is a
/// raw paint/present/poll loop over `int 0x80`.
fn build_pid4() -> Vec<u8> {
    let mut a = Asm::new();

    // boot sleep, matching the other three tasks
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(BOOT_SLEEP_TICKS);
    a.int80();

    // initial fill colour lives in r8d (survives every syscall)
    a.mov_r8d(0x00_30_58_20);

    // CREATE: SYS_GUI(rdi = 0, rsi = &CreateReq)
    a.mov_edi(0);
    let create_req = a.mov_esi_abs();
    a.mov_eax(SYS_GUI);
    a.int80();

    let cycle = a.buf.len();

    // refill the buffer: eax = colour, edx = base, ecx = pixel count
    a.mov_eax_r8d();
    let fill_base = a.mov_edx_abs();
    a.mov_ecx_imm(GUI_PIXELS);
    let fill_loop = a.buf.len();
    a.store_edx_eax();
    a.add_edx_imm8(4);
    a.dec_ecx();
    a.jnz_back(fill_loop);

    // PRESENT: SYS_GUI(rdi = 1) — kernel recomposites the dirty window
    a.mov_edi(1);
    a.mov_eax(SYS_GUI);
    a.int80();

    // pace so the colour rotation is visible (6 ticks = ~0.06 s)
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(GUI_SLEEP_TICKS);
    a.int80();

    a.add_r8d_imm32(GUI_COLOR_STEP);

    // GET_EVENT: SYS_GUI(rdi = 2) — drain one queued input event, if any
    a.mov_edi(2);
    a.mov_eax(SYS_GUI);
    a.int80();
    a.test_rax_rax();
    let jz_no_event = a.jz_fwd();
    a.mov_ecx_eax();
    a.and_ecx_ff();
    a.cmp_ecx_imm8(1); // EV_KEY
    let je_key = a.je_fwd();
    a.cmp_ecx_imm8(2); // EV_CLICK
    let je_click = a.je_fwd();
    let jmp_no_1 = a.jmp_fwd();

    // click: switch the fill to green and echo the event to the kernel log
    a.patch_rel8(je_click);
    a.mov_r8d(GUI_CLICK_COLOR);
    a.mov_esi_placeholder(2);
    a.mov_eax(SYS_WRITE);
    a.int80();
    let jmp_no_2 = a.jmp_fwd();

    // key: switch the fill to red and echo the event to the kernel log
    a.patch_rel8(je_key);
    a.mov_r8d(GUI_KEY_COLOR);
    a.mov_esi_placeholder(1);
    a.mov_eax(SYS_WRITE);
    a.int80();

    a.patch_rel8(jz_no_event);
    a.patch_rel8(jmp_no_1);
    a.patch_rel8(jmp_no_2);
    a.jmp_back(cycle);

    // ---- data section (offsets resolved after the layout is final) ----
    let title_off = a.buf.len();
    a.string(b"ring3 client");
    a.string(b"ui key");
    a.string(b"ui click");
    let req_off = a.buf.len();
    a.data_u32(GUI_W as u32);
    a.data_u32(GUI_H as u32);
    let req_buf = a.buf.len();
    a.data_u32(0);
    let req_title = a.buf.len();
    a.data_u32(0);
    let buf_off = a.buf.len();
    for _ in 0..GUI_PIXELS {
        a.data_u32(0x00_10_18_20);
    }

    let base = CODE_BASE + PID_D as u64 * REGION_STRIDE;
    a.patch_imm32(create_req, (base + req_off as u64) as u32);
    a.patch_imm32(fill_base, (base + buf_off as u64) as u32);
    a.patch_imm32(req_buf, (base + buf_off as u64) as u32);
    a.patch_imm32(req_title, (base + title_off as u64) as u32);
    a.finish(PID_D)
}

/// Ring-3 socket client (pid 5): a userspace loopback round over the `SYS_SOCK_*`
/// family. Two UDP sockets on fixed local ports talk to each other through the
/// kernel socket table and the loopback ring: `SYS_SOCK_ADDR` waits for a bound
/// IPv4 (0 while the link is down), connect(client → server port) +
/// `SYS_SOCK_SEND "ring3"`, a length-gated `SYS_SOCK_RECV`, then the reverse UDP
/// leg (`ack!`). Since v2.52.0 a **TCP round** follows: a TCP client connects to
/// a listening TCP socket, the listener hands out the accepted child and the
/// pair echo `tcp`/`ok`, then `SYS_SOCK_CLOSE` frees the child (the persistent
/// client/listener fds stay open across rounds). `SYS_WRITE` prints `sock-e ok`
/// / `sock-t ok`. Fds live in r8d/r9d (UDP) and r12d/r13d/r14d (TCP — client /
/// listener / accepted child, all surviving `int 0x80`), the IP snapshot in ebx;
/// esi/edx are per-call syscall arguments. Any failed handshake step retries
/// after a short sleep so the demo never spins.
fn build_pid5() -> Vec<u8> {
    let mut a = Asm::new();

    // boot sleep, matching the other tasks
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(BOOT_SLEEP_TICKS);
    a.int80();

    // open both UDP sockets and keep the fds in r8d (client) / r9d (server)
    a.mov_edi(0);
    a.mov_eax(SYS_SOCKET);
    a.int80();
    a.mov_r8d_eax();
    a.mov_edi(0);
    a.mov_eax(SYS_SOCKET);
    a.int80();
    a.mov_r9d_eax();

    // fixed local ports so the loopback 5-tuples are stable
    a.mov_edi_r8d();
    a.mov_esi_imm(SOCK_CLI_PORT);
    a.mov_eax(SYS_SOCK_BIND);
    a.int80();
    a.mov_edi_r9d();
    a.mov_esi_imm(SOCK_SRV_PORT);
    a.mov_eax(SYS_SOCK_BIND);
    a.int80();

    // open the TCP sockets and keep the fds in r12d (client) / r13d (listener)
    a.mov_edi(1);
    a.mov_eax(SYS_SOCKET);
    a.int80();
    a.mov_r12d_eax();
    a.mov_edi(1);
    a.mov_eax(SYS_SOCKET);
    a.int80();
    a.mov_r13d_eax();
    a.mov_edi_r12d();
    a.mov_esi_imm(SOCK_TCP_CLI_PORT);
    a.mov_eax(SYS_SOCK_BIND);
    a.int80();
    a.mov_edi_r13d();
    a.mov_esi_imm(SOCK_TCP_SRV_PORT);
    a.mov_eax(SYS_SOCK_BIND);
    a.int80();
    a.mov_edi_r13d();
    a.mov_eax(SYS_SOCK_LISTEN);
    a.int80();

    let start = a.buf.len();

    // wait for an address (link down -> SYS_SOCK_ADDR returns 0), then snapshot it in ebx
    a.mov_eax(SYS_SOCK_ADDR);
    a.int80();
    a.mov_ebx_eax();
    a.test_rax_rax();
    let jz_no_ip = a.jz_fwd();

    // leg 1: client sends "ring3" to the server's local port
    a.mov_edi_r8d();
    a.mov_esi_ebx();
    a.mov_edx_imm(SOCK_SRV_PORT);
    a.mov_eax(SYS_SOCK_CONNECT);
    a.int80();
    a.test_rax_rax();
    let jz_cfail = a.jz_fwd();
    a.mov_edi_r8d();
    a.mov_esi_placeholder(0);
    a.mov_edx_imm(SOCK_REQ_LEN);
    a.mov_eax(SYS_SOCK_SEND);
    a.int80();
    a.test_rax_rax();
    let jz_sfail = a.jz_fwd();

    // server leg: wait until exactly 5 syscall-bytes arrive (the "ring3" datagram)
    let rw1 = a.buf.len();
    a.mov_edi_r9d();
    let dbuf = a.mov_esi_abs();
    a.mov_edx_imm(SOCK_BUF_LEN);
    a.mov_eax(SYS_SOCK_RECV);
    a.int80();
    a.cmp_eax_imm8(SOCK_REQ_LEN as u8);
    let je_got1 = a.je_fwd();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(1);
    a.int80();
    a.jmp_back(rw1);
    a.patch_rel8(je_got1);

    // leg 2: the server replies "ack!" to the client's local port
    a.mov_edi_r9d();
    a.mov_esi_ebx();
    a.mov_edx_imm(SOCK_CLI_PORT);
    a.mov_eax(SYS_SOCK_CONNECT);
    a.int80();
    a.test_rax_rax();
    let jz_srvfail = a.jz_fwd();
    a.mov_edi_r9d();
    a.mov_esi_placeholder(1);
    a.mov_edx_imm(SOCK_REPLY_LEN);
    a.mov_eax(SYS_SOCK_SEND);
    a.int80();
    a.test_rax_rax();
    let jz_srvsfail = a.jz_fwd();

    // client leg: wait until exactly 4 syscall-bytes arrive (the "ack!" datagram)
    let rw2 = a.buf.len();
    a.mov_edi_r8d();
    let dbuf2 = a.mov_esi_abs();
    a.mov_edx_imm(SOCK_BUF_LEN);
    a.mov_eax(SYS_SOCK_RECV);
    a.int80();
    a.cmp_eax_imm8(SOCK_REPLY_LEN as u8);
    let je_got2 = a.je_fwd();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(1);
    a.int80();
    a.jmp_back(rw2);
    a.patch_rel8(je_got2);

    // round trip done (UDP): verdict
    a.mov_esi_placeholder(2);
    a.mov_eax(SYS_WRITE);
    a.int80();

    // ---- TCP round (v2.52.0): listen/connect/accept/echo over loopback ----
    // connect the TCP client to the listener's port, retrying until the SYN goes out
    let tconn = a.buf.len();
    a.mov_edi_r12d();
    a.mov_esi_ebx();
    a.mov_edx_imm(SOCK_TCP_SRV_PORT);
    a.mov_eax(SYS_SOCK_CONNECT);
    a.int80();
    a.test_rax_rax();
    let jne_tconn = a.jne_fwd();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(1);
    a.int80();
    a.jmp_back(tconn);
    a.patch_rel8(jne_tconn);

    // accept the listener until the handshake promotes a child (u64::MAX otherwise)
    let tacc = a.buf.len();
    a.mov_edi_r13d();
    a.mov_eax(SYS_SOCK_ACCEPT);
    a.int80();
    a.cmp_eax_imm8(0xFF);
    let jne_tacc = a.jne_fwd();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(1);
    a.int80();
    a.jmp_back(tacc);
    a.patch_rel8(jne_tacc);
    a.mov_r14d_eax();

    // client -> server: "tcp"
    let ts1 = a.buf.len();
    a.mov_edi_r12d();
    a.mov_esi_placeholder(4);
    a.mov_edx_imm(SOCK_TCP_MSG_LEN);
    a.mov_eax(SYS_SOCK_SEND);
    a.int80();
    a.test_rax_rax();
    let jne_ts1 = a.jne_fwd();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(1);
    a.int80();
    a.jmp_back(ts1);
    a.patch_rel8(jne_ts1);

    // child: wait until exactly 3 bytes arrive
    let rw_t1 = a.buf.len();
    a.mov_edi_r14d();
    let dbuf3 = a.mov_esi_abs();
    a.mov_edx_imm(SOCK_BUF_LEN);
    a.mov_eax(SYS_SOCK_RECV);
    a.int80();
    a.cmp_eax_imm8(SOCK_TCP_MSG_LEN as u8);
    let je_gt1 = a.je_fwd();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(1);
    a.int80();
    a.jmp_back(rw_t1);
    a.patch_rel8(je_gt1);

    // child -> client: "ok"
    let ts2 = a.buf.len();
    a.mov_edi_r14d();
    a.mov_esi_placeholder(5);
    a.mov_edx_imm(SOCK_TCP_REPLY_LEN);
    a.mov_eax(SYS_SOCK_SEND);
    a.int80();
    a.test_rax_rax();
    let jne_ts2 = a.jne_fwd();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(1);
    a.int80();
    a.jmp_back(ts2);
    a.patch_rel8(jne_ts2);

    // client: wait until exactly 2 bytes arrive
    let rw_t2 = a.buf.len();
    a.mov_edi_r12d();
    let dbuf4 = a.mov_esi_abs();
    a.mov_edx_imm(SOCK_BUF_LEN);
    a.mov_eax(SYS_SOCK_RECV);
    a.int80();
    a.cmp_eax_imm8(SOCK_TCP_REPLY_LEN as u8);
    let je_gt2 = a.je_fwd();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(1);
    a.int80();
    a.jmp_back(rw_t2);
    a.patch_rel8(je_gt2);

    // TCP round done: verdict, then free the accepted child. The
    // first `SYS_SOCK_CLOSE` sends a FIN (the slot lingers in
    // FinWait2 until the peer's FIN); the second one reclaims the
    // slot immediately, so every round returns its transient child
    // to the socket table.
    a.mov_esi_placeholder(3);
    a.mov_eax(SYS_WRITE);
    a.int80();
    a.mov_edi_r14d();
    a.mov_eax(SYS_SOCK_CLOSE);
    a.int80();
    a.mov_edi_r14d();
    a.mov_eax(SYS_SOCK_CLOSE);
    a.int80();
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(60);
    a.int80();
    a.patch_rel8(jz_no_ip);
    a.patch_rel8(jz_cfail);
    a.patch_rel8(jz_sfail);
    a.patch_rel8(jz_srvfail);
    a.patch_rel8(jz_srvsfail);
    a.mov_eax(SYS_SLEEP);
    a.mov_edi(20);
    a.int80();
    a.jmp_back(start);

    // ---- data section (offsets resolved after the layout is final) ----
    a.string(b"ring3");
    a.string(b"ack!");
    a.string(b"sock-e ok");
    a.string(b"sock-t ok");
    a.string(b"tcp");
    a.string(b"ok");
    let dbuf_off = a.buf.len();
    for _ in 0..(SOCK_BUF_LEN / 4) {
        a.data_u32(0);
    }
    let dbuf2_off = a.buf.len();
    for _ in 0..(SOCK_BUF_LEN / 4) {
        a.data_u32(0);
    }
    let dbuf3_off = a.buf.len();
    for _ in 0..(SOCK_BUF_LEN / 4) {
        a.data_u32(0);
    }
    let dbuf4_off = a.buf.len();
    for _ in 0..(SOCK_BUF_LEN / 4) {
        a.data_u32(0);
    }

    let base = CODE_BASE + PID_E as u64 * REGION_STRIDE;
    a.patch_imm32(dbuf, (base + dbuf_off as u64) as u32);
    a.patch_imm32(dbuf2, (base + dbuf2_off as u64) as u32);
    a.patch_imm32(dbuf3, (base + dbuf3_off as u64) as u32);
    a.patch_imm32(dbuf4, (base + dbuf4_off as u64) as u32);
    a.finish(PID_E)
}

fn spawn_one(pid: u32) -> Result<(), &'static str> {
    let code = match pid {
        PID_A => build_pid1(),
        PID_B => build_pid2(),
        PID_C => build_pid3(),
        PID_D => build_pid4(),
        PID_E => build_pid5(),
        _ => return Err("user: unknown pid"),
    };
    let code_va = CODE_BASE + pid as u64 * REGION_STRIDE;
    let stack_hi = STACK_TOP - pid as u64 * REGION_STRIDE;

    let pages = code.len() as u64 / memory::PAGE_SIZE + 1;
    for i in 0..pages {
        let frame = memory::alloc_frame().ok_or("user: out of frames for code")?;
        memory::map_page(code_va + i * memory::PAGE_SIZE, frame, true)?;
    }
    unsafe {
        let dst = code_va as *mut u8;
        for (i, b) in code.iter().enumerate() {
            dst.add(i).write_volatile(*b);
        }
    }

    for i in 0..STACK_PAGES {
        let frame = memory::alloc_frame().ok_or("user: out of frames for stack")?;
        let va = stack_hi - (i + 1) * memory::PAGE_SIZE;
        memory::map_page(va, frame, true)?;
        // zero the page so the first push lands on clean memory
        unsafe {
            let p = va as *mut u64;
            for w in 0..(memory::PAGE_SIZE / 8) {
                p.add(w as usize).write_volatile(0);
            }
        }
    }

    match sched::spawn_user(code_va, stack_hi, pid as u64, 0) {
        Some(slot) => {
            vprintln!(
                "[user] pid {} mapped at {:#x} -> sched slot {}",
                pid,
                code_va,
                slot
            );
            kprintln!("[serial] [user] pid {} spawned (slot {})", pid, slot);
            Ok(())
        }
        None => Err("user: no free scheduler slot"),
    }
}

/// Map + register the demo tasks.
///
/// Spawn order fixes the scheduler slot assignment (worker owns slot 1):
/// A = slot 2, B = slot 3, C = slot 4, D (the GUI client) = slot 5,
/// E (the socket client) = slot 6. The `SLOT_*` SEND/RCV targets above depend
/// on this exact order, so it must not change.
pub fn init() -> Result<(), &'static str> {
    spawn_one(PID_A)?;
    spawn_one(PID_B)?;
    spawn_one(PID_C)?;
    spawn_one(PID_D)?;
    spawn_one(PID_E)?;
    vprintln!("Milestone 3-5 userspace armed: five ring-3 tasks");
    kprintln!("[serial] [user] five ring-3 tasks armed (A/B/C + GUI + sock client)");
    Ok(())
}
