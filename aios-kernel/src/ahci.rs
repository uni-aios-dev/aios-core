//! AHCI (SATA) block driver.
//!
//! Drives SATA disks behind an AHCI controller discovered through `pci`. The
//! controller BAR is mapped with `memory::map_mmio` (the boot HHDM does not
//! cover MMIO) while the DMA frames are reached through the HHDM. Only polled
//! command issue (no interrupts) is implemented, which is enough for boot-time
//! block access: `IDENTIFY DEVICE` plus `READ DMA EXT`.

use crate::memory;
use crate::pci::{self, PciDevice};
use core::ptr;

const HBA_PORT_BASE: u64 = 0x100;
const HBA_PORT_STRIDE: u64 = 0x80;

const REG_GHC: u64 = 0x04;
const REG_PI: u64 = 0x0C;

/// Global HBA control: HBA reset.
const GHC_HR: u32 = 1 << 0;
/// Global HBA control: AHCI enable.
const GHC_AE: u32 = 1 << 31;

/// Port command: start processing.
const PXCMD_ST: u32 = 1 << 0;
/// Port command: FIS receive enable.
const PXCMD_FRE: u32 = 1 << 4;
/// Port command: FIS receive running.
const PXCMD_FR: u32 = 1 << 14;
/// Port command: command list running.
const PXCMD_CR: u32 = 1 << 15;

/// Port interrupt status: host bus fatal error.
const PXIS_HBFS: u32 = 1 << 29;
/// Port interrupt status: task file error.
const PXIS_TFES: u32 = 1 << 30;

const PORTOFF_CLB: u64 = 0x00;
const PORTOFF_CLBU: u64 = 0x04;
const PORTOFF_FB: u64 = 0x08;
const PORTOFF_FBU: u64 = 0x0C;
const PORTOFF_IS: u64 = 0x10;
const PORTOFF_CMD: u64 = 0x18;
const PORTOFF_SIG: u64 = 0x24;
const PORTOFF_SSTS: u64 = 0x28;
const PORTOFF_SERR: u64 = 0x30;
const PORTOFF_CI: u64 = 0x38;

const SATA_SIG_ATAPI: u32 = 0xEB14_0101;
const SATA_SIG_SEMB: u32 = 0xC33C_0101;
const SATA_SIG_PM: u32 = 0x9669_0101;

const FIS_TYPE_H2D: u8 = 0x27;
const ATA_CMD_IDENTIFY: u8 = 0xEC;
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;

const SECTOR_SIZE: usize = 512;
const MAX_DRIVES: usize = 4;
const SPIN_LIMIT: u32 = 20_000_000;

/// A SATA disk identified on one AHCI port.
#[derive(Clone, Copy)]
pub struct AhciDrive {
    /// Zero-based HBA port the disk is attached to.
    pub port: u8,
    /// Raw `IDENTIFY DEVICE` model string, trailing blanks trimmed.
    pub model: [u8; 40],
    /// Number of valid bytes in `model`.
    pub model_len: usize,
    /// Total addressable 512-byte sectors.
    pub sectors: u64,
    clb_phys: u64,
    ct_phys: u64,
}

impl AhciDrive {
    /// Blank slot used to size the fixed drive array without allocating.
    pub const EMPTY: AhciDrive = AhciDrive {
        port: 0,
        model: [0; 40],
        model_len: 0,
        sectors: 0,
        clb_phys: 0,
        ct_phys: 0,
    };

    /// The disk model as a UTF-8 string (falls back to `"?"` on bad bytes).
    pub fn model_str(&self) -> &str {
        core::str::from_utf8(&self.model[..self.model_len]).unwrap_or("?")
    }
}

/// A probed AHCI controller and the disks attached to its ports.
pub struct Ahci {
    abar: u64,
    data_phys: u64,
    drives: [AhciDrive; MAX_DRIVES],
    drive_count: usize,
}

impl Ahci {
    /// Probes an AHCI PCI function, bringing its HBA out of reset and
    /// identifying every SATA disk on an implemented port.
    pub fn init(dev: &PciDevice) -> Result<Ahci, &'static str> {
        let bar5 = dev.bars[5];
        if bar5 & 0x1 != 0 {
            return Err("ahci: BAR5 is an I/O BAR");
        }
        let abar_phys = (bar5 & 0xFFFF_FFF0) as u64;
        if abar_phys == 0 {
            return Err("ahci: BAR5 is zero");
        }
        unsafe {
            let command = pci::config_read32(dev.bus, dev.device, dev.function, 0x04);
            pci::config_write32(dev.bus, dev.device, dev.function, 0x04, command | 0x6);
        }
        let abar = memory::map_mmio(abar_phys, 0x2000)?;

        hba_write(abar, REG_GHC, hba_read(abar, REG_GHC) | GHC_HR);
        spin_until(|| hba_read(abar, REG_GHC) & GHC_HR == 0).map_err(|_| "ahci: reset timeout")?;
        hba_write(abar, REG_GHC, hba_read(abar, REG_GHC) | GHC_AE);

        let mut ahci = Ahci {
            abar,
            data_phys: 0,
            drives: [AhciDrive::EMPTY; MAX_DRIVES],
            drive_count: 0,
        };
        ahci.data_phys = memory::alloc_frame().ok_or("ahci: no DMA frame")?;
        zero_frame(ahci.data_phys);

        let implemented = hba_read(abar, REG_PI);
        for port in 0u8..32 {
            if implemented & (1 << port) == 0 {
                continue;
            }
            if ahci.drive_count >= MAX_DRIVES {
                break;
            }
            if let Some(drive) = ahci.probe_port(port) {
                ahci.drives[ahci.drive_count] = drive;
                ahci.drive_count += 1;
            }
        }
        Ok(ahci)
    }

    /// Disks identified by [`Ahci::init`].
    pub fn drives(&self) -> &[AhciDrive] {
        &self.drives[..self.drive_count]
    }

    /// Reads `count` 512-byte sectors starting at `lba` into `out`.
    ///
    /// Reads are issued one sector at a time into a single DMA frame and then
    /// copied out, so `out` may live in ordinary (non-physical) kernel memory.
    pub fn read_sectors(
        &self,
        drive_index: usize,
        lba: u64,
        count: usize,
        out: &mut [u8],
    ) -> Result<(), &'static str> {
        if drive_index >= self.drive_count {
            return Err("ahci: drive index out of range");
        }
        if out.len() < count * SECTOR_SIZE {
            return Err("ahci: output buffer too small");
        }
        let drive = &self.drives[drive_index];
        if lba + count as u64 > drive.sectors {
            return Err("ahci: read past end of drive");
        }
        let base = port_reg(self.abar, drive.port, 0);
        let scratch = memory::physical_to_virtual(self.data_phys);
        for offset in 0..count {
            let current = lba + offset as u64;
            let mut fis = [0u8; 20];
            fis[0] = FIS_TYPE_H2D;
            fis[1] = 0x80;
            fis[2] = ATA_CMD_READ_DMA_EXT;
            fis[4] = current as u8;
            fis[5] = (current >> 8) as u8;
            fis[6] = (current >> 16) as u8;
            fis[7] = 0x40;
            fis[8] = (current >> 24) as u8;
            fis[9] = (current >> 32) as u8;
            fis[10] = (current >> 40) as u8;
            fis[12] = 1;
            unsafe {
                issue(
                    base,
                    drive.clb_phys,
                    drive.ct_phys,
                    &fis,
                    self.data_phys,
                    SECTOR_SIZE as u32,
                    false,
                )?;
                let src = scratch as *const u8;
                for i in 0..SECTOR_SIZE {
                    out[offset * SECTOR_SIZE + i] = ptr::read_volatile(src.add(i));
                }
            }
        }
        Ok(())
    }

    fn probe_port(&self, port: u8) -> Option<AhciDrive> {
        let base = port_reg(self.abar, port, 0);
        if pread(base, PORTOFF_SSTS) & 0xF != 3 {
            return None;
        }

        let clb_phys = memory::alloc_frame()?;
        let fis_phys = memory::alloc_frame()?;
        let ct_phys = memory::alloc_frame()?;
        zero_frame(clb_phys);
        zero_frame(fis_phys);
        zero_frame(ct_phys);

        unsafe { stop_port(base) };
        pwrite(base, PORTOFF_CLB, clb_phys as u32);
        pwrite(base, PORTOFF_CLBU, (clb_phys >> 32) as u32);
        pwrite(base, PORTOFF_FB, fis_phys as u32);
        pwrite(base, PORTOFF_FBU, (fis_phys >> 32) as u32);
        pwrite(base, PORTOFF_SERR, 0xFFFF_FFFF);
        let command = pread(base, PORTOFF_CMD);
        pwrite(base, PORTOFF_CMD, command | PXCMD_FRE);
        pwrite(base, PORTOFF_CMD, command | PXCMD_FRE | PXCMD_ST);
        let _ = spin_until(|| pread(base, PORTOFF_SSTS) & 0xF == 3);

        let signature = pread(base, PORTOFF_SIG);
        if matches!(signature, SATA_SIG_ATAPI | SATA_SIG_SEMB | SATA_SIG_PM) {
            return None;
        }

        let mut fis = [0u8; 20];
        fis[0] = FIS_TYPE_H2D;
        fis[1] = 0x80;
        fis[2] = ATA_CMD_IDENTIFY;
        unsafe {
            issue(
                base,
                clb_phys,
                ct_phys,
                &fis,
                self.data_phys,
                SECTOR_SIZE as u32,
                false,
            )
            .ok()?
        };

        let identify = memory::physical_to_virtual(self.data_phys) as *const u8;
        let identify = unsafe { core::slice::from_raw_parts(identify, SECTOR_SIZE) };
        let mut drive = AhciDrive {
            port,
            model: [0; 40],
            model_len: 40,
            sectors: 0,
            clb_phys,
            ct_phys,
        };
        drive.model.copy_from_slice(&identify[54..94]);
        // ATA strings swap the two bytes of every 16-bit word.
        let mut pair = 0;
        while pair + 1 < drive.model_len {
            drive.model.swap(pair, pair + 1);
            pair += 2;
        }
        while drive.model_len > 0 {
            let last = drive.model[drive.model_len - 1];
            if last != b' ' && last != 0 {
                break;
            }
            drive.model_len -= 1;
        }
        drive.sectors = if read_u16(identify, 83) & (1 << 10) != 0 {
            read_u64_words(identify, 100)
        } else {
            read_u64_words(identify, 60) & 0x0FFF_FFFF
        };
        Some(drive)
    }
}

unsafe fn issue(
    base: u64,
    clb_phys: u64,
    ct_phys: u64,
    fis: &[u8; 20],
    buf_phys: u64,
    bytes: u32,
    write: bool,
) -> Result<(), &'static str> {
    let ct = memory::physical_to_virtual(ct_phys) as *mut u8;
    for i in 0..64 {
        ptr::write_volatile(ct.add(i), 0);
    }
    for (i, byte) in fis.iter().enumerate() {
        ptr::write_volatile(ct.add(i), *byte);
    }
    let prdt = ct.add(128) as *mut u32;
    ptr::write_volatile(prdt, buf_phys as u32);
    ptr::write_volatile(prdt.add(1), (buf_phys >> 32) as u32);
    ptr::write_volatile(prdt.add(2), 0);
    ptr::write_volatile(prdt.add(3), bytes - 1);

    let clb = memory::physical_to_virtual(clb_phys) as *mut u32;
    let flags = 5u32 | if write { 1 << 6 } else { 0 } | (1 << 16);
    ptr::write_volatile(clb, flags);
    ptr::write_volatile(clb.add(1), 0);
    ptr::write_volatile(clb.add(2), ct_phys as u32);
    ptr::write_volatile(clb.add(3), (ct_phys >> 32) as u32);

    pwrite(base, PORTOFF_IS, 0xFFFF_FFFF);
    pwrite(base, PORTOFF_CI, 1);
    spin_until(|| pread(base, PORTOFF_CI) & 1 == 0).map_err(|_| "ahci: command timeout")?;
    if pread(base, PORTOFF_IS) & (PXIS_TFES | PXIS_HBFS) != 0 {
        return Err("ahci: command failed");
    }
    Ok(())
}

unsafe fn stop_port(base: u64) {
    let mut command = pread(base, PORTOFF_CMD);
    pwrite(base, PORTOFF_CMD, command & !PXCMD_ST);
    let _ = spin_until(|| pread(base, PORTOFF_CMD) & PXCMD_CR == 0);
    command = pread(base, PORTOFF_CMD);
    pwrite(base, PORTOFF_CMD, command & !PXCMD_FRE);
    let _ = spin_until(|| pread(base, PORTOFF_CMD) & PXCMD_FR == 0);
}

fn spin_until<F: FnMut() -> bool>(mut condition: F) -> Result<(), ()> {
    for _ in 0..SPIN_LIMIT {
        if condition() {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(())
}

fn hba_read(abar: u64, offset: u64) -> u32 {
    unsafe { ptr::read_volatile((abar + offset) as *const u32) }
}

fn hba_write(abar: u64, offset: u64, value: u32) {
    unsafe { ptr::write_volatile((abar + offset) as *mut u32, value) }
}

fn pread(base: u64, offset: u64) -> u32 {
    unsafe { ptr::read_volatile((base + offset) as *const u32) }
}

fn pwrite(base: u64, offset: u64, value: u32) {
    unsafe { ptr::write_volatile((base + offset) as *mut u32, value) }
}

fn port_reg(abar: u64, port: u8, offset: u64) -> u64 {
    abar + HBA_PORT_BASE + (port as u64) * HBA_PORT_STRIDE + offset
}

fn zero_frame(phys: u64) {
    let virt = memory::physical_to_virtual(phys) as *mut u8;
    unsafe { ptr::write_bytes(virt, 0, memory::PAGE_SIZE as usize) };
}

fn read_u16(buf: &[u8], word: usize) -> u16 {
    (buf[word * 2] as u16) | ((buf[word * 2 + 1] as u16) << 8)
}

fn read_u64_words(buf: &[u8], word: usize) -> u64 {
    (read_u16(buf, word) as u64)
        | ((read_u16(buf, word + 1) as u64) << 16)
        | ((read_u16(buf, word + 2) as u64) << 32)
        | ((read_u16(buf, word + 3) as u64) << 48)
}
