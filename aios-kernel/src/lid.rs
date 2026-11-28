//! Lid open/closed detection by probing Embedded Controller RAM.
//!
//! ACPI exposes no portable lid-state register: on laptops the switch lives as
//! a bit somewhere inside the EC RAM. This module snapshots the full 256-byte
//! EC RAM at boot, then polls it in a ring; the first observed bit transition
//! becomes the "lid candidate", and its live value is published as 0/1. Until
//! the first transition `known` stays false and the TUI shows a scanning state.
//! Boards without an answering EC stay inactive and the status is "n/a".

use crate::kprintln;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Number of EC RAM offsets re-checked per poll cycle.
const SCAN_WINDOW: usize = 8;
/// Raw EC bit value that means "lid open". Set to `false` for boards whose
/// switch is active-low (verify with the `[lid] flip off=… bit=…` serial line).
const LID_OPEN_LEVEL: bool = true;

/// True once the EC answered a RAM sweep in [`init`].
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// True once a candidate bit was pinned (first observed flip).
static KNOWN: AtomicBool = AtomicBool::new(false);
/// Current value of the candidate bit (0/1).
static LID_OPEN: AtomicBool = AtomicBool::new(false);
/// EC RAM offset holding the candidate bit.
static CAND_OFFSET: AtomicU32 = AtomicU32::new(0);
/// Bit index of the candidate within its byte.
static CAND_BIT: AtomicU32 = AtomicU32::new(0);

static mut BASELINE: [u8; 256] = [0; 256];
static mut CURSOR: usize = 0;
/// Number of EC RAM bytes that answered during [`init`]; bounds the scan ring
/// so unread offsets (which stay zero in the baseline) cannot pin a false bit.
static EC_BYTES: AtomicU32 = AtomicU32::new(0);

/// Whether the lid heuristic is live.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Whether a candidate lid bit was pinned after at least one transition.
pub fn known() -> bool {
    KNOWN.load(Ordering::Relaxed)
}

/// Raw value of the candidate bit (only meaningful when [`known`]).
pub fn lid_open() -> bool {
    LID_OPEN.load(Ordering::Relaxed)
}

/// EC RAM offset of the candidate bit.
pub fn candidate_offset() -> u32 {
    CAND_OFFSET.load(Ordering::Relaxed)
}

/// Bit index of the candidate within its EC RAM byte.
pub fn candidate_bit() -> u32 {
    CAND_BIT.load(Ordering::Relaxed)
}

/// Captures the EC RAM baseline. Must run after [`crate::acpi::init`];
/// requires the FADT to be present so a machine without ACPI does not probe.
pub fn init() {
    if !crate::acpi::acpi_found() {
        kprintln!("[serial] [lid] skipped: no FADT");
        return;
    }
    unsafe {
        #[allow(static_mut_refs)]
        let baseline = &mut *core::ptr::addr_of_mut!(BASELINE);
        let n = crate::ec::probe(baseline);
        EC_BYTES.store(n as u32, Ordering::Relaxed);
        if n < 16 {
            kprintln!("[serial] [lid] ec unresponsive ({} bytes), lid n/a", n);
            ACTIVE.store(false, Ordering::Relaxed);
            return;
        }
        kprintln!(
            "[serial] [lid] ec active, {} bytes baseline, scanning window {}, open level {}",
            n,
            SCAN_WINDOW,
            LID_OPEN_LEVEL as u8
        );
        ACTIVE.store(crate::ec::active(), Ordering::Relaxed);
        KNOWN.store(false, Ordering::Relaxed);
        *core::ptr::addr_of_mut!(CURSOR) = 0;
    }
}

/// Re-checks a slice of the EC RAM. Called periodically from the idle loop;
/// cheap (1-8 bounded EC byte reads). After a candidate is pinned only that
/// byte is refreshed so the lid value stays live.
pub fn poll() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    unsafe {
        if KNOWN.load(Ordering::Relaxed) {
            let off = CAND_OFFSET.load(Ordering::Relaxed) as u8;
            if let Some(v) = crate::ec::read_ram(off) {
                let raw = ((v >> CAND_BIT.load(Ordering::Relaxed)) & 1) != 0;
                LID_OPEN.store(raw == LID_OPEN_LEVEL, Ordering::Relaxed);
            }
            return;
        }
        let limit = EC_BYTES.load(Ordering::Relaxed) as usize;
        if limit == 0 {
            return;
        }
        let base = &mut *core::ptr::addr_of_mut!(BASELINE);
        let start = *core::ptr::addr_of!(CURSOR);
        let cursor = core::ptr::addr_of_mut!(CURSOR);
        for i in 0..SCAN_WINDOW {
            let off = (start + i) % limit;
            let Some(v) = crate::ec::read_ram(off as u8) else {
                continue;
            };
            let old = base[off];
            if v == old {
                continue;
            }
            let mut bit = 0usize;
            for b in 0..8usize {
                if (v >> b) & 1 != (old >> b) & 1 {
                    bit = b;
                    break;
                }
            }
            let raw = ((v >> bit) & 1) != 0;
            let open = raw == LID_OPEN_LEVEL;
            KNOWN.store(true, Ordering::Relaxed);
            LID_OPEN.store(open, Ordering::Relaxed);
            CAND_OFFSET.store(off as u32, Ordering::Relaxed);
            CAND_BIT.store(bit as u32, Ordering::Relaxed);
            base[off] = v;
            kprintln!(
                "[serial] [lid] flip off=0x{:02x} byte 0x{:02x}->0x{:02x} bit={} raw={} lid={}",
                off,
                old,
                v,
                bit,
                raw as u8,
                open as u8
            );
        }
        *cursor = (start + SCAN_WINDOW) % limit;
    }
}
