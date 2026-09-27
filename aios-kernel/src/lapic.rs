//! Local APIC timer driver.
//!
//! Modern UEFI boards often leave the legacy 8259 PIC dead (IRQ0 is routed
//! through an unprogrammed IO-APIC), so the PIT interrupt never reaches the
//! CPU. The PIT countdown itself still runs, but a real *hardware tick* needs
//! the Local APIC timer. This module brings the LAPIC up, calibrates its
//! bus-frequency countdown against the PIT, and arms a periodic 100 Hz tick on
//! a dedicated vector (0x90) that lives outside the PIC range (0x20..=0x2F).
//!
//! Register access prefers x2APIC (MSR-based, no MMIO caching pitfalls) and
//! falls back to xAPIC MMIO at the base reported by `IA32_APIC_BASE` when the
//! CPU exposes neither x2APIC capable nor an enabled x2APIC mode.

use crate::interrupts::{pit_count, APIC_TIMER_VECTOR, TIMER_HZ};
use crate::{kprintln, memory};
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const IA32_APIC_BASE_MSR: u32 = 0x1B;
const APIC_ENABLE: u64 = 1 << 8;
const X2APIC_ENABLE: u64 = 1 << 10;

// xAPIC MMIO offsets.
const MMIO_SVR: u64 = 0xF0;
const MMIO_EOI: u64 = 0xB0;
const MMIO_LVT_TIMER: u64 = 0x320;
const MMIO_DIVIDE: u64 = 0x3E0;
const MMIO_INIT_COUNT: u64 = 0x380;
const MMIO_CURRENT_COUNT: u64 = 0x390;

// x2APIC MSRs.
const MSR_SVR: u32 = 0x80F;
const MSR_EOI: u32 = 0x80B;
const MSR_LVT_TIMER: u32 = 0x832;
const MSR_DIVIDE: u32 = 0x83E;
const MSR_INIT_COUNT: u32 = 0x838;
const MSR_CURRENT_COUNT: u32 = 0x839;

const LVT_PERIODIC: u32 = 1 << 17;
const SVR_ENABLE: u32 = 1 << 8;
const SVR_SPURIOUS: u32 = 0xFF;

/// Set once the periodic LAPIC timer is live; drives the TUI clock-mode label.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Access mode chosen at init (true = x2APIC MSR, false = xAPIC MMIO).
static X2APIC: AtomicBool = AtomicBool::new(false);
/// Mapped virtual base of the xAPIC MMIO region (unused in x2APIC mode).
static APIC_MMIO: AtomicU64 = AtomicU64::new(0);

/// Whether the Local APIC timer is driving ticks.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Brings the Local APIC timer up and arms a periodic 100 Hz tick on the
/// dedicated vector 0x90. Returns true once the timer is live.
///
/// The caller is expected to mask the PIT IRQ0 in the PIC afterwards so a
/// single hardware tick source drives the scheduler; the PIT *countdown*
/// remains available for `delay_ms`/calibration regardless of routing.
pub fn init() -> bool {
    let leaf1 = cpuid(1);
    if leaf1.3 & (1 << 9) == 0 {
        kprintln!("[serial] [lapic] no local APIC (cpuid edx bit9)");
        return false;
    }
    let mut apic_base = rdmsr(u64::from(IA32_APIC_BASE_MSR));
    let mut x2 = (apic_base & X2APIC_ENABLE) != 0;
    if !x2 && leaf1.2 & (1 << 21) != 0 {
        apic_base |= APIC_ENABLE | X2APIC_ENABLE;
        wrmsr(u64::from(IA32_APIC_BASE_MSR), apic_base);
        x2 = true;
        kprintln!("[serial] [lapic] x2APIC enabled via IA32_APIC_BASE");
    }

    if x2 {
        X2APIC.store(true, Ordering::Relaxed);
        kprintln!(
            "[serial] [lapic] x2APIC mode, base=0x{:016x}",
            apic_base & 0xFFFF_F000
        );
    } else {
        let phys = apic_base & 0xFFFF_F000;
        let virt = match memory::map_mmio(phys, 0x1000) {
            Ok(v) => v,
            Err(e) => {
                kprintln!("[serial] [lapic] map_mmio({:#x}) failed: {}", phys, e);
                return false;
            }
        };
        APIC_MMIO.store(virt, Ordering::Relaxed);
        kprintln!("[serial] [lapic] xAPIC mode mmio={:#x}", virt);
    }

    write_reg(MMIO_SVR, MSR_SVR, SVR_ENABLE | SVR_SPURIOUS);

    let Some(count) = calibrate() else {
        kprintln!("[serial] [lapic] calibration failed, keeping PIT");
        return false;
    };

    write_reg(
        MMIO_LVT_TIMER,
        MSR_LVT_TIMER,
        APIC_TIMER_VECTOR as u32 | LVT_PERIODIC,
    );
    write_reg(MMIO_INIT_COUNT, MSR_INIT_COUNT, count);
    ACTIVE.store(true, Ordering::Relaxed);
    kprintln!(
        "[serial] [lapic] periodic {} Hz tick armed: vector=0x{:02x} init_count={}",
        TIMER_HZ,
        APIC_TIMER_VECTOR,
        count
    );
    true
}

/// Acknowledges the LAPIC interrupt (mandatory before returning from the ISR).
pub fn eoi() {
    if X2APIC.load(Ordering::Relaxed) {
        wrmsr(u64::from(MSR_EOI), 0);
    } else {
        let base = APIC_MMIO.load(Ordering::Relaxed);
        unsafe {
            core::ptr::write_volatile((base + MMIO_EOI) as *mut u32, 0);
        }
    }
}

/// Measures the LAPIC countdown rate against the PIT and returns the 32-bit
/// initial count that produces a `TIMER_HZ`-period periodic tick with divide
/// config 1.
fn calibrate() -> Option<u32> {
    write_reg(MMIO_DIVIDE, MSR_DIVIDE, 0xB);
    write_reg(MMIO_INIT_COUNT, MSR_INIT_COUNT, u32::MAX);
    let c0 = read_reg(MMIO_CURRENT_COUNT, MSR_CURRENT_COUNT);
    let pit0 = wait_pit(10);
    let c1 = read_reg(MMIO_CURRENT_COUNT, MSR_CURRENT_COUNT);

    let elapsed = c0.wrapping_sub(c1);
    if elapsed == 0 || pit0 == 0 {
        return None;
    }
    let bus_hz = u64::from(elapsed)
        .checked_mul(1_193_182)
        .map(|v| v / pit0)?;
    let count = bus_hz / TIMER_HZ;
    if count == 0 || count > u64::from(u32::MAX) {
        return None;
    }
    kprintln!(
        "[serial] [lapic] calib: bus_hz={} pit_delta={} lapic_delta={}",
        bus_hz,
        pit0,
        elapsed
    );
    Some(count as u32)
}

/// Busy-waits `ms` milliseconds against the PIT countdown and returns the
/// number of PIT counts that elapsed (the countdown always runs).
fn wait_pit(ms: u64) -> u64 {
    let desired = (1_193_182u64 / 1000) * ms;
    let mut last = pit_count();
    let mut elapsed = 0u64;
    loop {
        let now = pit_count();
        elapsed += u64::from(last.wrapping_sub(now));
        last = now;
        if elapsed >= desired {
            return elapsed;
        }
        core::hint::spin_loop();
    }
}

fn read_reg(mmio: u64, msr: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        rdmsr(u64::from(msr)) as u32
    } else {
        let base = APIC_MMIO.load(Ordering::Relaxed);
        unsafe { core::ptr::read_volatile((base + mmio) as *const u32) }
    }
}

fn write_reg(mmio: u64, msr: u32, value: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        wrmsr(u64::from(msr), u64::from(value));
    } else {
        let base = APIC_MMIO.load(Ordering::Relaxed);
        unsafe {
            core::ptr::write_volatile((base + mmio) as *mut u32, value);
        }
    }
}

fn rdmsr(msr: u64) -> u64 {
    let hi: u32;
    let lo: u32;
    unsafe {
        asm!("rdmsr", out("eax") lo, out("edx") hi, in("ecx") msr, options(nomem, nostack));
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

fn wrmsr(msr: u64, value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    unsafe {
        asm!("wrmsr", in("eax") lo, in("edx") hi, in("ecx") msr, options(nomem, nostack));
    }
}

/// Runs `CPUID` on `leaf` and returns (eax, ebx, ecx, edx).
///
/// `rbx` is a LLVM-reserved register, so it is preserved around the
/// instruction with a temp instead of being named as an asm operand.
fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let mut eax = leaf;
    let mut ecx: u32;
    let mut edx: u32;
    let mut tmp: u64;
    unsafe {
        asm!(
            "mov {tmp:e}, ebx",
            "cpuid",
            "xchg {tmp:e}, ebx",
            inout("eax") eax,
            out("ecx") ecx,
            out("edx") edx,
            tmp = out(reg) tmp,
            options(nostack)
        );
    }
    (eax, tmp as u32, ecx, edx)
}
