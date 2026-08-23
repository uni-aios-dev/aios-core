//! Ring-3 demo programs and their memory setup (Milestones 3+4).
//!
//! Two tiny user programs are copied into freshly mapped user pages and
//! entered through the scheduler's fabricated ring-3 frames. They talk to
//! each other through the `int 0x80` IPC syscalls:
//!
//! - program A (pid 1): sends its counter to pid 2, then drains its inbox;
//! - program B (pid 2): mirrors A with the roles swapped.
//!
//! The kernel logs a sample of the traffic plus the first syscall of every
//! task, which together prove ring-3 execution, syscall gating, preemption
//! and IPC delivery in one QEMU run.

use alloc::vec::Vec;

use crate::ipc::{SYS_RECV, SYS_SEND};
use crate::memory;
use crate::sched;
use crate::{kprintln, vprintln};

pub const PID_A: u32 = 1;
pub const PID_B: u32 = 2;

/// Per-task stride so code/stack regions never overlap.
const REGION_STRIDE: u64 = 0x20_0000; // 2 MiB per task
const CODE_BASE: u64 = 0x4000_0000;
const STACK_TOP: u64 = 0x7F00_0000;
const STACK_PAGES: u64 = 4;

fn build_program(dst: u32) -> Vec<u8> {
    let mut p = Vec::new();
    if dst == PID_B {
        // recv first for B: eax=2, edi=dst(self mailbox is indexed by pid)
        extend(&mut p, &[0xB8]); imm32(&mut p, SYS_RECV);
        extend(&mut p, &[0x31, 0xFF]); // xor edi,edi -> recv own mailbox
        extend(&mut p, &[0xCD, 0x80]);
        // then send counter to peer A
        extend(&mut p, &[0xB8]); imm32(&mut p, SYS_SEND);
        extend(&mut p, &[0xBF]); imm32(&mut p, PID_A as u64);
        extend(&mut p, &[0xFF, 0xC1]); // inc ecx
        extend(&mut p, &[0xCD, 0x80]);
    } else {
        // send first for A
        extend(&mut p, &[0xB8]); imm32(&mut p, SYS_SEND);
        extend(&mut p, &[0xBF]); imm32(&mut p, dst as u64);
        extend(&mut p, &[0xFF, 0xC1]); // inc ecx
        extend(&mut p, &[0xCD, 0x80]);
        // then drain inbox
        extend(&mut p, &[0xB8]); imm32(&mut p, SYS_RECV);
        extend(&mut p, &[0x31, 0xFF]);
        extend(&mut p, &[0xCD, 0x80]);
    }
    // pacing delay: r8d = 0x0040_0000 iterations
    extend(&mut p, &[0x41, 0xB8]); imm32(&mut p, 0x0040_0000);
    let dec_at = p.len();
    extend(&mut p, &[0x41, 0xFF, 0xC8]); // dec r8d
    extend(&mut p, &[0x75]); // jnz rel8 -> dec
    p.push((dec_at as i64 - (p.len() + 1) as i64) as u8);
    extend(&mut p, &[0xEB, 0x00]); // jmp rel8 back to 0 (patched below)
    let end = p.len() as i64;
    let last = p.len() - 1;
    p[last] = (-(end)) as i64 as u8;
    p
}

fn extend(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
}

fn imm32(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

fn spawn_one(pid: u32) -> Result<(), &'static str> {
    let code = build_program(if pid == PID_A { PID_B } else { PID_A });
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
            vprintln!("[user] pid {} mapped at {:#x} -> sched slot {}", pid, code_va, slot);
            kprintln!("[serial] [user] pid {} spawned (slot {})", pid, slot);
            Ok(())
        }
        None => Err("user: no free scheduler slot"),
    }
}

/// Map + register both demo tasks.
pub fn init() -> Result<(), &'static str> {
    spawn_one(PID_A)?;
    spawn_one(PID_B)?;
    vprintln!("Milestone 3/4 userspace armed: two ring-3 IPC tasks");
    kprintln!("[serial] [user] both ring-3 tasks armed");
    Ok(())
}
