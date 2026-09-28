//! CPU digital thermal sensor (DTS): TjMax + digital readout over model-specific
//! registers, plus thermal protection gating.
//!
//! Detection gates on the DTS feature bit (CPUID leaf 1 EDX bit 22). On capable
//! silicon, `IA32_TEMPERATURE_TARGET` (0x1A2) supplies TjMax and
//! `IA32_THERM_STATUS` (0x19C) a 7-bit delta below TjMax; the live temperature
//! is `TjMax - delta`. When DTS is absent (QEMU with a non-feature CPU, older
//! parts) a synthetic triangular wave is produced so the scheduler gate and the
//! TUI still exercise the full path. [`critical`] drives the scheduler: while
//! the temperature is at or above the threshold, ring-3 user tasks are paused.

use core::sync::atomic::{AtomicBool, AtomicI16, AtomicU8, Ordering};
use crate::kprintln;

/// Protection gate: temperatures at or above this pause ring-3 tasks.
pub const THRESHOLD_C: i16 = 90;
/// Lowest simulated temperature for the DTS-less fallback.
const SIM_MIN_C: i16 = 45;
/// Highest simulated temperature for the DTS-less fallback.
const SIM_MAX_C: i16 = 92;

const MSR_TEMPERATURE_TARGET: u32 = 0x1A2;
const MSR_THERM_STATUS: u32 = 0x19C;

/// Live die temperature in degrees Celsius.
static TEMP_C: AtomicI16 = AtomicI16::new(0);
/// TjMax from the package, in degrees Celsius.
static TJMAX_C: AtomicU8 = AtomicU8::new(0);
/// True when real DTS registers are being read.
static DTS_AVAIL: AtomicBool = AtomicBool::new(false);
/// True when a synthetic temperature is shown (no DTS on this CPU).
static SIMULATED: AtomicBool = AtomicBool::new(true);
/// Cross-transition bookkeeping for the one-shot serial logs.
static WAS_CRITICAL: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// #GP-safe RDMSR.
//
// QEMU advertises the DTS feature bit yet raises #GP(0) on the thermal MSRs,
// and a handful of early boards behave the same. A tiny assembly stub runs the
// read; if the CPU faults, the vector-13 arm of the interrupt dispatcher
// redirects the frame to `_aios_probe_rdmsr_fault` and the probe reports a
// miss. See `probe_active` / `on_probe_gp`.
core::arch::global_asm!(
    ".text",
    ".globl _aios_probe_rdmsr",
    "_aios_probe_rdmsr:",
    "    mov ecx, edi",
    "    rdmsr",
    "    shl rdx, 32",
    "    or  rax, rdx",
    "    ret",
    ".globl _aios_probe_rdmsr_fault",
    "_aios_probe_rdmsr_fault:",
    "    xor eax, eax",
    "    xor edx, edx",
    "    ret"
);
extern "C" {
    fn _aios_probe_rdmsr(msr: u32) -> u64;
    fn _aios_probe_rdmsr_fault();
}

/// Set while a guarded RDMSR is in flight; the interrupt dispatcher checks this
/// before printing a fatal on vector 13.
static PROBE_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Set by the #GP redirect when the probed MSR raised a fault.
static PROBE_FAULTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn probe_active() -> bool {
    PROBE_ACTIVE.load(Ordering::Relaxed)
}

/// Runs the RDMSR inside the guarded stub; `None` when the MSR is not
/// supported on this CPU.
fn try_rdmsr(msr: u32) -> Option<u64> {
    unsafe {
        PROBE_FAULTED.store(false, Ordering::Relaxed);
        PROBE_ACTIVE.store(true, Ordering::Relaxed);
        let v = _aios_probe_rdmsr(msr);
        PROBE_ACTIVE.store(false, Ordering::Relaxed);
        if PROBE_FAULTED.load(Ordering::Relaxed) {
            None
        } else {
            Some(v)
        }
    }
}

/// Called from the vector-13 arm of the interrupt dispatcher while a probing
/// RDMSR is in flight: marks the probe as faulted and resumes just past it.
/// Must not be called at any other time.
pub(crate) fn on_probe_gp(frame: &mut crate::interrupts::InterruptFrame) {
    PROBE_FAULTED.store(true, Ordering::Relaxed);
    PROBE_ACTIVE.store(false, Ordering::Relaxed);
    frame.rip = _aios_probe_rdmsr_fault as *const () as u64;
    frame.error_code = 0;
}

/// Live temperature in degrees Celsius.
pub fn temp_c() -> i16 {
    TEMP_C.load(Ordering::Relaxed)
}

/// TjMax in degrees Celsius (0 when unknown).
pub fn tjmax_c() -> u8 {
    TJMAX_C.load(Ordering::Relaxed)
}

/// Whether the displayed temperature is synthetic (no DTS support).
pub fn simulated() -> bool {
    SIMULATED.load(Ordering::Relaxed)
}

/// True while the die is at or above [`THRESHOLD_C`]; the scheduler uses this
/// to pause ring-3 tasks.
pub fn critical() -> bool {
    TEMP_C.load(Ordering::Relaxed) >= THRESHOLD_C
}

fn real_temp() -> i16 {
    let tjmax = TJMAX_C.load(Ordering::Relaxed) as i16;
    if tjmax == 0 {
        return 0;
    }
    let delta = try_rdmsr(MSR_THERM_STATUS)
        .map(|v| ((v >> 16) & 0x7F) as i16)
        .unwrap_or(0);
    (tjmax - delta).clamp(0, 150)
}

/// Triangle wave for DTS-less platforms: rises SIM_MIN..SIM_MAX one degree per
/// second, then falls back down — the gate engages and releases periodically.
fn sim_temp(ticks: u64) -> i16 {
    let rise = (SIM_MAX_C - SIM_MIN_C) as u64;
    let period = 2 * rise;
    let t = (ticks / crate::interrupts::TIMER_HZ) % period;
    if t < rise {
        SIM_MIN_C + t as i16
    } else {
        SIM_MAX_C - (t - rise) as i16
    }
}

/// Probes CPUID/MSRs once and takes the first reading. Copies of the values
/// stay valid for the whole boot; only `poll` refreshes them.
pub fn init() {
    let cpu = core::arch::x86_64::__cpuid(1);
    let dts = cpu.edx & (1 << 22) != 0;
    if dts {
        if let Some(target) = try_rdmsr(MSR_TEMPERATURE_TARGET) {
            let tjmax = ((target >> 16) & 0xFF) as u8;
            if tjmax != 0 && tjmax <= 200 {
                TJMAX_C.store(tjmax, Ordering::Relaxed);
                DTS_AVAIL.store(true, Ordering::Relaxed);
                SIMULATED.store(false, Ordering::Relaxed);
                TEMP_C.store(real_temp(), Ordering::Relaxed);
                kprintln!(
                    "[serial] [thermal] dts tjmax={} temp={} (msr 0x{:x}/0x{:x})",
                    tjmax,
                    temp_c(),
                    MSR_TEMPERATURE_TARGET,
                    MSR_THERM_STATUS
                );
                return;
            }
            kprintln!(
                "[serial] [thermal] dts flag set but TjMax invalid ({}), falling back to sim",
                tjmax
            );
        } else {
            kprintln!(
                "[serial] [thermal] dts flag set but MSR 0x{:x} faults, falling back to sim",
                MSR_TEMPERATURE_TARGET
            );
        }
    }
    SIMULATED.store(true, Ordering::Relaxed);
    TEMP_C.store(sim_temp(0), Ordering::Relaxed);
    kprintln!(
        "[serial] [thermal] no dts, simulated temp {}..{} (gate {}C engaged for demo)",
        SIM_MIN_C,
        SIM_MAX_C,
        THRESHOLD_C
    );
}

/// Refreshes the temperature and logs gate crossings. Called periodically from
/// the idle loop; the MSR reads cost a few hundred cycles.
pub fn poll() {
    let ticks = crate::interrupts::TICKS.load(Ordering::Relaxed);
    let t = if DTS_AVAIL.load(Ordering::Relaxed) && !SIMULATED.load(Ordering::Relaxed) {
        real_temp()
    } else {
        sim_temp(ticks)
    };
    TEMP_C.store(t, Ordering::Relaxed);
    let crit = t >= THRESHOLD_C;
    let was = WAS_CRITICAL.swap(crit, Ordering::Relaxed);
    if crit && !was {
        kprintln!("[serial] [thermal] CRITICAL temp={} (ring-3 paused)", t);
    } else if !crit && was {
        kprintln!("[serial] [thermal] normal temp={} (ring-3 resumed)", t);
    }
}