//! Minimal ACPI table walk (RSDP -> XSDT/RSDT -> FADT).
//!
//! The kernel needs two firmware facts: are ACPI tables present at all, and is
//! there an embedded controller (EC) to probe for the laptop lid switch. A
//! signature + revision pass over the root and system tables yields the FADT;
//! its presence gates the EC RAM probing in [`crate::ec`]. The ACPI EC is not
//! listed in the FADT — its I/O registers are the fixed ports 0x62/0x66 on
//! essentially every mobile board — so the FADT is used as a liveness gate for
//! probing those fixed ports. Table addresses are physical; they are reached
//! through the bootloader HHDM window via [`crate::memory::physical_to_virtual`].

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use crate::kprintln;

/// ACPI Embedded Controller data port (byte reads / writes).
pub const EC_DATA_PORT: u16 = 0x62;
/// ACPI Embedded Controller status / command port.
pub const EC_STATUS_PORT: u16 = 0x66;

/// True once the FADT ("FACP") was located in the system tables.
static FADT_FOUND: AtomicBool = AtomicBool::new(false);
/// Physical address of the DSDT extracted from the FADT (0 when absent).
static DSDT_ADDR: AtomicU32 = AtomicU32::new(0);
/// ACPI revision reported by the FADT (master tables revision).
static ACPI_REV: AtomicU32 = AtomicU32::new(0);

/// Whether the ACPI walk succeeded far enough to trust the EC probing.
pub fn acpi_found() -> bool {
    FADT_FOUND.load(Ordering::Relaxed)
}

fn rd_u8(addr: u64) -> u8 {
    unsafe { core::ptr::read_volatile(addr as *const u8) }
}

fn rd_u32(addr: u64) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn rd_u64(addr: u64) -> u64 {
    unsafe { core::ptr::read_volatile(addr as *const u64) }
}

/// Maps an ACPI table address to its virtual alias.
///
/// Limine reports the RSDP pointer already HHDM-offset on some builds and as a
/// raw physical address on others, so either form is accepted here; addresses
/// read out of the tables themselves (XSDT/RSDT entry slots, FADT fields) are
/// always physical and go through the plain HHDM offset.
fn virt_addr(addr: u64) -> u64 {
    let off = crate::memory::physical_to_virtual(0);
    if addr >= off {
        addr
    } else {
        off + addr
    }
}

/// Walks one root system table (XSDT with 8-byte entries, or RSDT with 4-byte
/// entries) and returns the physical address of the entry whose header
/// signature equals `want`.
fn find_entry(root_phys: u64, entry_size: usize, want: &[u8; 4]) -> Option<u64> {
    let root_p = crate::memory::physical_to_virtual(root_phys);
    let len = rd_u32(root_p + 4) as usize;
    let n = len.saturating_sub(36) / entry_size;
    let mut slot = 36usize;
    let mut found = None;
    for _ in 0..n {
        let addr = if entry_size == 8 {
            rd_u64(root_p + slot as u64)
        } else {
            rd_u32(root_p + slot as u64) as u64
        };
        if addr != 0 {
            let p = crate::memory::physical_to_virtual(addr);
            if rd_u8(p) == want[0]
                && rd_u8(p + 1) == want[1]
                && rd_u8(p + 2) == want[2]
                && rd_u8(p + 3) == want[3]
            {
                found = Some(addr);
                break;
            }
        }
        slot += entry_size;
    }
    found
}

/// Walks the ACPI table tree starting at the Limine-provided RSDP physical
/// address and publishes the FADT/DSDT facts. Cheap and side-effect free
/// besides the serial log lines.
pub fn init(rsdp_phys: u64) {
    let p = virt_addr(rsdp_phys);
    let sig_ok = rd_u8(p) == b'R'
        && rd_u8(p + 1) == b'S'
        && rd_u8(p + 2) == b'D'
        && rd_u8(p + 3) == b' '
        && rd_u8(p + 4) == b'P'
        && rd_u8(p + 5) == b'T'
        && rd_u8(p + 6) == b'R'
        && rd_u8(p + 7) == b' ';
    if !sig_ok {
        kprintln!("[serial] [acpi] bad RSDP signature at 0x{:x}", rsdp_phys);
        return;
    }
    let rev = rd_u8(p + 15);
    let (root, entry_size, kind): (u64, usize, &str) = if rev >= 2 {
        let xsdt = rd_u64(p + 24);
        if xsdt == 0 {
            kprintln!("[serial] [acpi] revision={} xsdt=0 (absent)", rev);
            return;
        }
        (xsdt, 8, "XSDT")
    } else {
        (rd_u32(p + 16) as u64, 4, "RSDT")
    };
    kprintln!(
        "[serial] [acpi] rev={} {}@0x{:x}",
        rev,
        kind,
        root
    );
    let root_p = crate::memory::physical_to_virtual(root);
    let root_sig_ok = (kind == "XSDT"
        && rd_u8(root_p) == b'X'
        && rd_u8(root_p + 1) == b'S'
        && rd_u8(root_p + 2) == b'D'
        && rd_u8(root_p + 3) == b'T')
        || (kind == "RSDT"
            && rd_u8(root_p) == b'R'
            && rd_u8(root_p + 1) == b'S'
            && rd_u8(root_p + 2) == b'D'
            && rd_u8(root_p + 3) == b'T');
    if !root_sig_ok {
        kprintln!("[serial] [acpi] {} signature mismatch", kind);
        return;
    }
    let Some(fadt) = find_entry(root, entry_size, b"FACP") else {
        kprintln!("[serial] [acpi] FADT not found");
        return;
    };
    let fp = crate::memory::physical_to_virtual(fadt);
    let fadt_rev = rd_u8(fp + 8) as u32;
    let dsdt = if fadt_rev >= 2 { rd_u32(fp + 36) } else { 0 };
    FADT_FOUND.store(true, Ordering::Relaxed);
    ACPI_REV.store(fadt_rev, Ordering::Relaxed);
    DSDT_ADDR.store(dsdt, Ordering::Relaxed);
    kprintln!(
        "[serial] [acpi] fadt rev={} phys=0x{:x} dsdt=0x{:08x} ec-ports=0x{:02x}/0x{:02x}",
        fadt_rev,
        fadt,
        dsdt,
        EC_DATA_PORT,
        EC_STATUS_PORT
    );
}