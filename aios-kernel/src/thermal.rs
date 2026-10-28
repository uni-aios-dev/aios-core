//! CPU digital thermal sensor (DTS): TjMax + digital readout over model-specific
//! registers, plus thermal protection gating.
//!
//! Detection gates on CPUID.06H:EAX[0] (falling back to the older leaf-1
//! EDX bit 22 marker). On capable
//! silicon, `IA32_TEMPERATURE_TARGET` (0x1A2) supplies TjMax and
//! `IA32_THERM_STATUS` (0x19C) a 7-bit delta below TjMax; the live temperature
//! is `TjMax - delta`. When the readout cannot be trusted (no DTS bit, the MSR
//! faults - QEMU advertises DTS yet raises #GP - or the status word marks the
//! readout invalid) a synthetic wave is produced so the TUI still has a value
//! to draw. Since v2.68.4 that value is clearly marked (`~` in the status bar,
//! `sim=1` in the System tab) and never drives the scheduler gate. [`critical`]
//! pauses ring-3 user tasks only on real DTS readings at or above the threshold.

use crate::kprintln;
use core::sync::atomic::{AtomicBool, AtomicI16, AtomicU8, Ordering};

/// Protection gate: real DTS temperatures at or above this pause ring-3 tasks.
pub const THRESHOLD_C: i16 = 90;
/// Lowest simulated temperature for the DTS-less fallback.
const SIM_MIN_C: i16 = 45;
/// Highest simulated temperature for the DTS-less fallback (below the gate, so
/// a synthetic value can never pause user tasks).
const SIM_MAX_C: i16 = 60;
/// TjMax assumed when `IA32_TEMPERATURE_TARGET` is unavailable.
const DEFAULT_TJMAX_C: u8 = 100;

const MSR_TEMPERATURE_TARGET: u32 = 0x1A2;
const MSR_THERM_STATUS: u32 = 0x19C;

/// Live die temperature in degrees Celsius.
static TEMP_C: AtomicI16 = AtomicI16::new(0);
/// TjMax from the package, in degrees Celsius.
static TJMAX_C: AtomicU8 = AtomicU8::new(0);
/// True while real DTS registers are being read.
static DTS_AVAIL: AtomicBool = AtomicBool::new(false);
/// True when a synthetic temperature is shown (no usable DTS on this CPU).
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

/// Whether the displayed temperature is synthetic (no usable DTS).
pub fn simulated() -> bool {
    SIMULATED.load(Ordering::Relaxed)
}

/// True while a *real* DTS reading is at or above [`THRESHOLD_C`]; a synthetic
/// value never pauses user tasks.
pub fn critical() -> bool {
    !SIMULATED.load(Ordering::Relaxed) && TEMP_C.load(Ordering::Relaxed) >= THRESHOLD_C
}

/// Reads `TjMax - delta` from MSR 0x19C, returning `None` when the MSR faults,
/// the status word reports the readout invalid (bit 31 = 0, SDM 14.7.5.2) or the delta is
/// implausible — never a made-up 0 C or TjMax reading.
fn read_dts() -> Option<i16> {
    let tjmax = TJMAX_C.load(Ordering::Relaxed) as i16;
    if tjmax == 0 {
        return None;
    }
    let status = try_rdmsr(MSR_THERM_STATUS)?;
    if status & (1 << 31) == 0 {
        return None;
    }
    let delta = ((status >> 16) & 0x7F) as i16;
    if delta > 100 {
        return None;
    }
    Some((tjmax - delta).clamp(0, 150))
}

/// Triangle wave for DTS-less platforms: rises SIM_MIN..SIM_MAX one degree per
/// second, then falls back down. It stays below the gate, is displayed with a
/// `~` marker and never pauses ring-3 tasks.
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
    // DTS enumeration is CPUID.06H:EAX[0] (SDM 14.7.5.1); older parts that
    // predate leaf 6 still advertise the ACPI thermal monitor in leaf 1 EDX[22].
    let max_leaf = core::arch::x86_64::__cpuid(0).eax;
    let dts = (max_leaf >= 6 && core::arch::x86_64::__cpuid(6).eax & 1 != 0)
        || core::arch::x86_64::__cpuid(1).edx & (1 << 22) != 0;
    if dts {
        let mut defaulted = false;
        let tjmax = match try_rdmsr(MSR_TEMPERATURE_TARGET).map(|v| ((v >> 16) & 0xFF) as u8) {
            Some(t) if t != 0 && t <= 125 => t,
            _ => {
                defaulted = true;
                kprintln!(
                    "[serial] [thermal] TjMax from MSR 0x{:x} unavailable, assuming {}C",
                    MSR_TEMPERATURE_TARGET,
                    DEFAULT_TJMAX_C
                );
                DEFAULT_TJMAX_C
            }
        };
        TJMAX_C.store(tjmax, Ordering::Relaxed);
        if let Some(t) = read_dts() {
            DTS_AVAIL.store(true, Ordering::Relaxed);
            SIMULATED.store(false, Ordering::Relaxed);
            TEMP_C.store(t, Ordering::Relaxed);
            kprintln!(
                "[serial] [thermal] dts tjmax={}{} temp={} (msr 0x{:x}/0x{:x})",
                tjmax,
                if defaulted { " (default)" } else { "" },
                t,
                MSR_TEMPERATURE_TARGET,
                MSR_THERM_STATUS
            );
            return;
        }
        kprintln!(
            "[serial] [thermal] dts flag set but MSR 0x{:x} unreadable, falling back to sim",
            MSR_THERM_STATUS
        );
    }
    SIMULATED.store(true, Ordering::Relaxed);
    TEMP_C.store(sim_temp(0), Ordering::Relaxed);
    kprintln!(
        "[serial] [thermal] no usable dts, simulated temp {}..{} ({}C gate applies to real readings only)",
        SIM_MIN_C,
        SIM_MAX_C,
        THRESHOLD_C
    );
}

/// Refreshes the temperature and logs gate crossings. Called periodically from
/// the idle loop; the MSR reads cost a few hundred cycles.
pub fn poll() {
    let ticks = crate::interrupts::TICKS.load(Ordering::Relaxed);
    let t = if DTS_AVAIL.load(Ordering::Relaxed) {
        match read_dts() {
            Some(v) => v,
            None => {
                DTS_AVAIL.store(false, Ordering::Relaxed);
                SIMULATED.store(true, Ordering::Relaxed);
                kprintln!("[serial] [thermal] dts readout lost, switching to simulated");
                sim_temp(ticks)
            }
        }
    } else {
        sim_temp(ticks)
    };
    TEMP_C.store(t, Ordering::Relaxed);
    let crit = critical();
    let was = WAS_CRITICAL.swap(crit, Ordering::Relaxed);
    if crit && !was {
        kprintln!("[serial] [thermal] CRITICAL temp={} (ring-3 paused)", t);
    } else if !crit && was {
        kprintln!("[serial] [thermal] normal temp={} (ring-3 resumed)", t);
    }
}
