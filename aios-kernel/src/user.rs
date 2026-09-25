//! Ring-3 demo programs and their memory setup (Milestones 3-5).
//!
//! Three tiny user programs are copied into freshly mapped user pages and
//! entered through the scheduler's fabricated ring-3 frames:
//!
//! - program A (pid 1): sends its counter to pid 2, drains its inbox, then
//!   prints `u1` to the kernel console through `SYS_WRITE`;
//! - program B (pid 2): mirrors A with the roles swapped (`u2`);
//! - program C (pid 3): prints `[usleep] up` and its pid via `SYS_GETPID`,
//!   then alternates `SYS_SLEEP(20)` with a `*` echo for 8 rounds.
//!
//! Every program starts by calling `SYS_SLEEP` (20 ticks) so the whole
//! userspace is briefly asleep at boot — the scheduler must fall back to the
//! idle context (`[sched] idle`) and wake the trio as their deadlines pass,
//! proving sleep, wake and idle fallback in one QEMU run.

use alloc::vec::Vec;

use crate::memory;
use crate::sched;
use crate::syscalls::{SYS_GETPID, SYS_RECV, SYS_SEND, SYS_SLEEP, SYS_WRITE};
use crate::{kprintln, vprintln};

pub const PID_A: u32 = 1;
pub const PID_B: u32 = 2;
pub const PID_C: u32 = 3;

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
    a.mov_edi(PID_B as u64);
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
    a.mov_edi(PID_A as u64);
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

fn spawn_one(pid: u32) -> Result<(), &'static str> {
    let code = match pid {
        PID_A => build_pid1(),
        PID_B => build_pid2(),
        PID_C => build_pid3(),
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

/// Map + register the three demo tasks.
///
/// Spawn order fixes the scheduler slot assignment (worker owns slot 1): A on
/// slot 2, C on slot 3 (the pid the smoke proofs watch) and B on slot 4.
pub fn init() -> Result<(), &'static str> {
    spawn_one(PID_A)?;
    spawn_one(PID_C)?;
    spawn_one(PID_B)?;
    vprintln!("Milestone 3-5 userspace armed: three ring-3 tasks");
    kprintln!("[serial] [user] three ring-3 tasks armed");
    Ok(())
}
