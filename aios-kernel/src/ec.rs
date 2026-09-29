//! ACPI Embedded Controller: byte-wide EC RAM reads over the fixed ports.
//!
//! The ACPI EC answers on I/O ports 0x62 (data) / 0x66 (status + command) (see
//! [`crate::acpi`]). A byte of the 256-byte EC RAM is fetched with the RD_EC
//! (0x80) command: wait for the input buffer to clear, issue the command and
//! the RAM address, then wait for the output buffer. Every wait is bounded so
//! a board without an EC (e.g. QEMU's default machine, whose unused port reads
//! back 0xFF) reports `active = false` instead of hanging the kernel.

use crate::kprintln;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;

/// EC data port selected by the last RD_EC address write.
const EC_DATA: u16 = crate::acpi::EC_DATA_PORT;
/// EC status/command port carrying the IBF/OBF flags.
const EC_STATUS: u16 = crate::acpi::EC_STATUS_PORT;
/// RD_EC: read one byte of EC RAM at `addr` (command to the status port).
const CMD_READ_EC: u8 = 0x80;
/// Input buffer full — the EC has not consumed the last byte yet.
const IBF: u8 = 0x02;
/// Output buffer full — a byte is waiting on the data port.
const OBF: u8 = 0x01;

/// Whether the EC answered a RAM read within its timeout.
static EC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// True once the EC has answered at least one byte.
pub fn active() -> bool {
    EC_ACTIVE.load(Ordering::Relaxed)
}

fn ec_status() -> u8 {
    unsafe { crate::port::inb(EC_STATUS) }
}

/// Waits for the EC input buffer to clear; bounded so an absent EC cannot hang
/// the kernel.
fn wait_ibf_clear() -> bool {
    for _ in 0..1_000_000 {
        if ec_status() & IBF == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Waits for the EC output buffer to fill; bounded like [`wait_ibf_clear`].
fn wait_obf_set() -> bool {
    for _ in 0..1_000_000 {
        if ec_status() & OBF != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Reads one byte of EC RAM at `addr`. Returns `None` when the EC never
/// answered (absent device), never a wrong byte.
pub fn read_ram(addr: u8) -> Option<u8> {
    unsafe {
        if !wait_ibf_clear() {
            return None;
        }
        crate::port::outb(EC_STATUS, CMD_READ_EC);
        if !wait_ibf_clear() {
            return None;
        }
        crate::port::outb(EC_DATA, addr);
        if !wait_obf_set() {
            return None;
        }
        let v = crate::port::inb(EC_DATA);
        EC_ACTIVE.store(true, Ordering::Relaxed);
        Some(v)
    }
}

/// Reads the whole 256-byte EC RAM into `buf`; returns the number of bytes
/// actually obtained (the first `None` aborts the sweep).
pub fn dump_full(buf: &mut [u8; 256]) -> usize {
    for (i, slot) in buf.iter_mut().enumerate() {
        match read_ram(i as u8) {
            Some(v) => *slot = v,
            None => return i,
        }
    }
    256
}

/// Probes the EC once: if a single byte answers, the RAM sweep is dumped to
/// the serial log in 32-byte rows so the lid heuristic and later calibration
/// have the full snapshot available.
pub fn probe() -> usize {
    let mut buf = [0u8; 256];
    let n = dump_full(&mut buf);
    if n == 0 {
        kprintln!(
            "[serial] [ec] no response on 0x{:02x}/0x{:02x}",
            EC_DATA,
            EC_STATUS
        );
        return 0;
    }
    kprintln!("[serial] [ec] active, {} bytes of EC RAM captured", n);
    for (row, chunk) in buf.chunks(16).enumerate() {
        let mut line = alloc::format!("[serial] [ec] {:04x}:", row * 16);
        for b in chunk {
            line.push_str(&alloc::format!(" {:02x}", b));
        }
        kprintln!("{}", line);
    }
    n
}
