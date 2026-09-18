//! NVMe block driver.
//!
//! Drives a single-namespace NVMe controller discovered through `pci`: it
//! brings the controller out of reset, sets up the admin queue, identifies the
//! controller and namespace 1, creates one I/O queue pair and moves blocks with
//! the `Read` command. Registers are reached through `memory::map_mmio`; queue
//! and data buffers are physically-contiguous frames reached through the HHDM.
//! Only polled command issue (no interrupts) is implemented.

use crate::memory;
use crate::pci::{self, PciDevice};
use core::ptr;
use core::sync::atomic::{fence, Ordering};

const REG_CAP: u64 = 0x00;
const REG_CC: u64 = 0x14;
const REG_CSTS: u64 = 0x1C;
const REG_AQA: u64 = 0x24;
const REG_ASQ: u64 = 0x28;
const REG_ACQ: u64 = 0x30;
const REG_INTMS: u64 = 0x0C;

const CC_EN: u32 = 1 << 0;
const CSTS_RDY: u32 = 1 << 0;
const CSTS_CFS: u32 = 1 << 1;

const ADMIN_QID: u32 = 0;
const IO_QID: u32 = 1;
const QUEUE_DEPTH: u32 = 8;

const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const IO_READ: u8 = 0x02;

const IDENTIFY_CTRL: u32 = 1;
const IDENTIFY_NS: u32 = 0;

const SQE_SIZE: usize = 64;
const CQE_SIZE: usize = 16;
const SPIN_LIMIT: u32 = 20_000_000;

/// An NVMe namespace (logical disk) exposed by the controller.
#[derive(Clone, Copy)]
pub struct NvmeDrive {
    /// ASCII model number (`MN`, 40 bytes, trailing blanks trimmed).
    pub model: [u8; 40],
    /// Number of valid bytes in `model`.
    pub model_len: usize,
    /// ASCII serial number (`SN`, 20 bytes, trailing blanks trimmed).
    pub serial: [u8; 20],
    /// Number of valid bytes in `serial`.
    pub serial_len: usize,
    /// Total addressable logical blocks.
    pub blocks: u64,
    /// Logical block size in bytes (from the namespace LBA format).
    pub lba_size: u32,
}

impl NvmeDrive {
    /// Blank slot used before identification.
    pub const EMPTY: NvmeDrive = NvmeDrive {
        model: [0; 40],
        model_len: 0,
        serial: [0; 20],
        serial_len: 0,
        blocks: 0,
        lba_size: 0,
    };

    /// The model as a UTF-8 string (falls back to `"?"` on bad bytes).
    pub fn model_str(&self) -> &str {
        core::str::from_utf8(&self.model[..self.model_len]).unwrap_or("?")
    }

    /// The serial as a UTF-8 string (falls back to `"?"` on bad bytes).
    pub fn serial_str(&self) -> &str {
        core::str::from_utf8(&self.serial[..self.serial_len]).unwrap_or("?")
    }

    /// Namespace capacity in bytes.
    pub fn bytes(&self) -> u64 {
        self.blocks * self.lba_size as u64
    }
}

/// Submission/completion ring pair state for one NVMe queue.
struct Queue {
    qid: u32,
    depth: u32,
    sq: u64,
    cq: u64,
    tail: u32,
    head: u32,
    phase: bool,
}

impl Queue {
    const fn new(qid: u32, depth: u32, sq: u64, cq: u64) -> Queue {
        Queue {
            qid,
            depth,
            sq,
            cq,
            tail: 0,
            head: 0,
            phase: true,
        }
    }
}

/// A probed NVMe controller and its single namespace.
pub struct Nvme {
    regs: u64,
    dstrd: u32,
    admin: Queue,
    io: Queue,
    data_phys: u64,
    drive: NvmeDrive,
}

impl Nvme {
    /// Probes an NVMe PCI function, brings the controller online and identifies
    /// namespace 1.
    pub fn init(dev: &PciDevice) -> Result<Nvme, &'static str> {
        if dev.bars[0] & 0x1 != 0 {
            return Err("nvme: BAR0 is an I/O BAR");
        }
        let bar_phys = ((dev.bars[0] & 0xFFFF_FFF0) as u64) | ((dev.bars[1] as u64) << 32);
        if bar_phys == 0 {
            return Err("nvme: BAR0 is zero");
        }
        unsafe {
            let command = pci::config_read32(dev.bus, dev.device, dev.function, 0x04);
            pci::config_write32(dev.bus, dev.device, dev.function, 0x04, command | 0x6);
        }
        let regs = memory::map_mmio(bar_phys, 0x2000)?;

        let cap = reg_read64(regs, REG_CAP);
        let dstrd = ((cap >> 32) & 0xF) as u32;

        reg_write32(regs, REG_INTMS, 0xFFFF_FFFF);

        if reg_read32(regs, REG_CC) & CC_EN != 0 {
            reg_write32(regs, REG_CC, reg_read32(regs, REG_CC) & !CC_EN);
            spin_until(|| reg_read32(regs, REG_CSTS) & CSTS_RDY == 0)
                .map_err(|_| "nvme: reset timeout")?;
        }
        spin_until(|| reg_read32(regs, REG_CSTS) & CSTS_RDY == 0)
            .map_err(|_| "nvme: not ready to configure")?;

        let asq = memory::alloc_frame().ok_or("nvme: no admin SQ frame")?;
        let acq = memory::alloc_frame().ok_or("nvme: no admin CQ frame")?;
        let iosq = memory::alloc_frame().ok_or("nvme: no io SQ frame")?;
        let iocq = memory::alloc_frame().ok_or("nvme: no io CQ frame")?;
        let data = memory::alloc_frame().ok_or("nvme: no data frame")?;
        for page in [asq, acq, iosq, iocq, data] {
            zero_frame(page);
        }

        // ASQS is bits 11:0 and ACQS bits 27:16.
        let aqa = (QUEUE_DEPTH - 1) | ((QUEUE_DEPTH - 1) << 16);
        reg_write32(regs, REG_AQA, aqa);
        reg_write64(regs, REG_ASQ, asq);
        reg_write64(regs, REG_ACQ, acq);

        // CSS = NVM (0), MPS = 4 KiB (0), IOSQES = 6 (64 B), IOCQES = 4 (16 B).
        reg_write32(regs, REG_CC, (6 << 16) | (4 << 20) | CC_EN);
        spin_until(|| {
            let csts = reg_read32(regs, REG_CSTS);
            csts & CSTS_RDY != 0 || csts & CSTS_CFS != 0
        })
        .map_err(|_| "nvme: enable timeout")?;
        if reg_read32(regs, REG_CSTS) & CSTS_CFS != 0 {
            return Err("nvme: controller fatal status");
        }

        let mut nvme = Nvme {
            regs,
            dstrd,
            admin: Queue::new(ADMIN_QID, QUEUE_DEPTH, asq, acq),
            io: Queue::new(IO_QID, QUEUE_DEPTH, iosq, iocq),
            data_phys: data,
            drive: NvmeDrive::EMPTY,
        };

        let mut sqe = [0u8; SQE_SIZE];
        build_identify(&mut sqe, 1, 0, data, IDENTIFY_CTRL);
        nvme.admin_submit(&sqe)?;
        {
            let ctrl = buffer(data);
            nvme.drive.model.copy_from_slice(&ctrl[24..64]);
            nvme.drive.model_len = trim_len(&nvme.drive.model, 40);
            nvme.drive.serial.copy_from_slice(&ctrl[4..24]);
            nvme.drive.serial_len = trim_len(&nvme.drive.serial, 20);
        }

        build_create_cq(&mut sqe, 2, iocq, IO_QID);
        nvme.admin_submit(&sqe)?;
        build_create_sq(&mut sqe, 3, iosq, IO_QID);
        nvme.admin_submit(&sqe)?;

        build_identify(&mut sqe, 4, 1, data, IDENTIFY_NS);
        nvme.admin_submit(&sqe)?;
        {
            let ns = buffer(data);
            let blocks = read_u64(ns, 0);
            let format = (ns[26] & 0x0F) as usize;
            let lbads = ns[128 + format * 4 + 2];
            if blocks == 0 {
                return Err("nvme: namespace 1 is empty");
            }
            if lbads == 0 || lbads > 16 {
                return Err("nvme: unsupported LBA size");
            }
            nvme.drive.blocks = blocks;
            nvme.drive.lba_size = 1u32 << lbads;
        }
        Ok(nvme)
    }

    /// The namespace exposed by the controller.
    pub fn drive(&self) -> &NvmeDrive {
        &self.drive
    }

    /// Reads `count` logical blocks starting at `lba` into `out`.
    ///
    /// One block is read per command into a single DMA frame and then copied
    /// out, so `out` may live in ordinary (non-physical) kernel memory.
    pub fn read_blocks(
        &mut self,
        lba: u64,
        count: usize,
        out: &mut [u8],
    ) -> Result<(), &'static str> {
        let block = self.drive.lba_size as usize;
        if block == 0 {
            return Err("nvme: namespace has no LBA size");
        }
        if out.len() < count * block {
            return Err("nvme: output buffer too small");
        }
        if lba + count as u64 > self.drive.blocks {
            return Err("nvme: read past end of namespace");
        }
        let mut sqe = [0u8; SQE_SIZE];
        for offset in 0..count {
            build_read(
                &mut sqe,
                (offset + 1) as u16,
                1,
                self.data_phys,
                lba + offset as u64,
                1,
            );
            Self::submit(self.regs, self.dstrd, &mut self.io, &sqe)?;
            let src = memory::physical_to_virtual(self.data_phys) as *const u8;
            for byte in 0..block {
                out[offset * block + byte] = unsafe { ptr::read_volatile(src.add(byte)) };
            }
        }
        Ok(())
    }

    fn admin_submit(&mut self, sqe: &[u8; SQE_SIZE]) -> Result<(), &'static str> {
        Self::submit(self.regs, self.dstrd, &mut self.admin, sqe)
    }

    fn submit(
        regs: u64,
        dstrd: u32,
        queue: &mut Queue,
        sqe: &[u8; SQE_SIZE],
    ) -> Result<(), &'static str> {
        let sq = memory::physical_to_virtual(queue.sq) as *mut u8;
        unsafe {
            ptr::copy_nonoverlapping(
                sqe.as_ptr(),
                sq.add(queue.tail as usize * SQE_SIZE),
                SQE_SIZE,
            );
        }
        queue.tail = (queue.tail + 1) % queue.depth;
        fence(Ordering::SeqCst);
        ring(regs, dstrd, queue.qid, false, queue.tail);

        let cq = memory::physical_to_virtual(queue.cq) as *const u8;
        let mut waited = 0u32;
        let status = loop {
            let entry = unsafe { cq.add(queue.head as usize * CQE_SIZE) };
            let status = unsafe { ptr::read_volatile(entry.add(12) as *const u32) };
            // The phase tag is bit 0 of the 16-bit status field at bits 31:16.
            if ((status >> 16) & 1 == 1) == queue.phase {
                break status;
            }
            waited += 1;
            if waited > SPIN_LIMIT {
                return Err("nvme: command timeout");
            }
            core::hint::spin_loop();
        };
        queue.head = (queue.head + 1) % queue.depth;
        if queue.head == 0 {
            queue.phase = !queue.phase;
        }
        ring(regs, dstrd, queue.qid, true, queue.head);

        // Bits 8:1 of the status field are the status code + code type.
        let status_field = (status >> 17) & 0x7FFF;
        if status_field != 0 {
            return Err("nvme: command failed");
        }
        Ok(())
    }
}

fn build_identify(sqe: &mut [u8; SQE_SIZE], cid: u16, nsid: u32, data_phys: u64, cns: u32) {
    sqe.fill(0);
    sqe[0] = ADMIN_IDENTIFY;
    put_u16(sqe, 2, cid);
    put_u32(sqe, 4, nsid);
    put_u64(sqe, 24, data_phys);
    put_u32(sqe, 40, cns);
}

fn build_create_cq(sqe: &mut [u8; SQE_SIZE], cid: u16, cq_phys: u64, qid: u32) {
    sqe.fill(0);
    sqe[0] = ADMIN_CREATE_CQ;
    put_u16(sqe, 2, cid);
    put_u64(sqe, 24, cq_phys);
    put_u32(sqe, 40, (qid & 0xFFFF) | ((QUEUE_DEPTH - 1) << 16));
    put_u32(sqe, 44, 1);
}

fn build_create_sq(sqe: &mut [u8; SQE_SIZE], cid: u16, sq_phys: u64, qid: u32) {
    sqe.fill(0);
    sqe[0] = ADMIN_CREATE_SQ;
    put_u16(sqe, 2, cid);
    put_u64(sqe, 24, sq_phys);
    put_u32(sqe, 40, (qid & 0xFFFF) | ((QUEUE_DEPTH - 1) << 16));
    put_u32(sqe, 44, 1 | (qid << 16));
}

fn build_read(
    sqe: &mut [u8; SQE_SIZE],
    cid: u16,
    nsid: u32,
    data_phys: u64,
    lba: u64,
    blocks: u32,
) {
    sqe.fill(0);
    sqe[0] = IO_READ;
    put_u16(sqe, 2, cid);
    put_u32(sqe, 4, nsid);
    put_u64(sqe, 24, data_phys);
    put_u32(sqe, 40, lba as u32);
    put_u32(sqe, 44, (lba >> 32) as u32);
    put_u32(sqe, 48, (blocks - 1) & 0xFFFF);
}

fn put_u16(buf: &mut [u8; SQE_SIZE], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8; SQE_SIZE], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buf: &mut [u8; SQE_SIZE], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn ring(regs: u64, dstrd: u32, qid: u32, completion: bool, value: u32) {
    let stride = 4u64 << dstrd;
    let offset = 0x1000 + (2 * qid as u64 + completion as u64) * stride;
    reg_write32(regs, offset, value);
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

fn reg_read32(regs: u64, offset: u64) -> u32 {
    unsafe { ptr::read_volatile((regs + offset) as *const u32) }
}

fn reg_write32(regs: u64, offset: u64, value: u32) {
    unsafe { ptr::write_volatile((regs + offset) as *mut u32, value) }
}

fn reg_read64(regs: u64, offset: u64) -> u64 {
    unsafe { ptr::read_volatile((regs + offset) as *const u64) }
}

fn reg_write64(regs: u64, offset: u64, value: u64) {
    unsafe { ptr::write_volatile((regs + offset) as *mut u64, value) }
}

fn zero_frame(phys: u64) {
    let virt = memory::physical_to_virtual(phys) as *mut u8;
    unsafe { ptr::write_bytes(virt, 0, memory::PAGE_SIZE as usize) };
}

fn buffer(phys: u64) -> &'static [u8] {
    let virt = memory::physical_to_virtual(phys) as *const u8;
    unsafe { core::slice::from_raw_parts(virt, memory::PAGE_SIZE as usize) }
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn trim_len(buf: &[u8], mut len: usize) -> usize {
    while len > 0 {
        let last = buf[len - 1];
        if last != b' ' && last != 0 {
            break;
        }
        len -= 1;
    }
    len
}
