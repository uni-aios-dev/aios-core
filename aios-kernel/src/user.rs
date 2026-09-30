//! Ring-3 demo programs and their memory setup (Milestones 3-5).
//!
//! Four tiny user programs are copied into freshly mapped user pages and
//! entered through the scheduler's fabricated ring-3 frames:
//!
//! - program A (slot 2): sends its counter to slot 3, drains its inbox, then
//!   prints `u1` to the kernel console through `SYS_WRITE`;
//! - program B (slot 3): mirrors A with the roles swapped (`u2`);
//! - program C (slot 4): prints `[usleep] up` and its pid via `SYS_GETPID`,
//!   then alternates `SYS_SLEEP(20)` with a `*` echo forever;
//! - program D (slot 5): the ring-3 GUI client — paints a 96x64 pixel buffer
//!   with a rotating solid colour and presents it to the kernel window server
//!   via `SYS_GUI` every 6 ticks (visible only while the GUI desktop owns the
//!   screen).
//!
//! `SYS_SEND` target ids are *scheduler slot ids* (slots 2/3/4 — the kernel
//! worker owns slot 1); `SLOT_*` constants document the mapping. A and B form
//! a ping-pong over the per-pid mailboxes whose sampled `[ipc] send/recv`
//! proof lines appear every 8th transferred value.
//!
//! Every program starts by calling `SYS_SLEEP` (20 ticks) so the whole
//! userspace is briefly asleep at boot — the scheduler must fall back to the
//! idle context (`[sched] idle`) and wake the four as their deadlines pass,
//! proving sleep, wake and idle fallback in one QEMU run.

use alloc::vec::Vec;

use crate::memory;
use crate::sched;
use crate::syscalls::{SYS_GETPID, SYS_GUI, SYS_RECV, SYS_SEND, SYS_SLEEP, SYS_WRITE};
use crate::{kprintln, vprintln};

pub const PID_A: u32 = 1;
pub const PID_B: u32 = 2;
pub const PID_C: u32 = 3;
pub const PID_D: u32 = 4;

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
/// each round. Uses no heap, no strings — a raw paint + present loop over the
/// `SYS_GUI` gate (create once at boot, `int 0x80` PRESENT in the loop).
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
    a.jmp_back(cycle);

    // ---- data section (offsets resolved after the layout is final) ----
    let title_off = a.buf.len();
    a.string(b"ring3 client");
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

fn spawn_one(pid: u32) -> Result<(), &'static str> {
    let code = match pid {
        PID_A => build_pid1(),
        PID_B => build_pid2(),
        PID_C => build_pid3(),
        PID_D => build_pid4(),
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
/// A = slot 2, B = slot 3, C = slot 4, D (the GUI client) = slot 5. The
/// `SLOT_*` SEND/RCV targets above depend on this exact order, so it must not
/// change.
pub fn init() -> Result<(), &'static str> {
    spawn_one(PID_A)?;
    spawn_one(PID_B)?;
    spawn_one(PID_C)?;
    spawn_one(PID_D)?;
    vprintln!("Milestone 3-5 userspace armed: four ring-3 tasks");
    kprintln!("[serial] [user] four ring-3 tasks armed (A/B/C + GUI client)");
    Ok(())
}
