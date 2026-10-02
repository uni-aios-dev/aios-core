//! Bare-metal NIC drivers polled from the idle loop (v2.38.34): Intel
//! PRO/1000 `e1000` (MMIO descriptor rings) and Realtek `rtl8139` (PIO
//! linear ring buffer). Both run without interrupts — the network stack
//! calls [`Nic::poll_rx`] every tick and [`Nic::send`] blocks bounded.
//!
//! Ring/buffer semantics were derived from the QEMU implementations
//! (`hw/net/e1000.c`, `hw/net/rtl8139.c`), which is the environment the
//! smoke harness runs in:
//!
//! * e1000 RX: descriptors `[RDH .. RDT-1]` are hardware-owned. After
//!   consuming descriptor `k` the driver re-primes it and writes `RDT = k`,
//!   leaving exactly one descriptor of slack (the ring never fully drains
//!   into `RDH == RDT` while the driver keeps consuming).
//! * e1000 TX: descriptor `t` is filled and handed over with `TDT = t+1`;
//!   `cmd = EOP|IFCS|RS` makes the device write `DD` back, which `send`
//!   waits for (bounded).
//! * rtl8139 RX: one 8 KiB linear buffer with hardware wrapping at the end
//!   (`RxConfig` bit 7 clear); `CAPR` is programmed as `read_pos - 16`
//!   (QEMU adds 16 back). Packets carry a 4-byte header (`len+4` in the
//!   high half) and a trailing CRC.
//! * rtl8139 TX: round-robin over four descriptors — the device only ever
//!   transmits `currTxDesc`, so the driver must fill them in order.

use crate::memory;
use crate::pci::{self, PciDevice};
use crate::port;
use core::ptr;
use core::sync::atomic::{fence, Ordering};

/// Largest Ethernet frame the stack parses (1500 MTU + 14 header, no FCS).
pub const FRAME_MAX: usize = 1514;

/// Number of descriptors in each e1000 ring (`128 / 16`).
const E1000_RING: usize = 8;
/// rtl8139 linear receive buffer size (8 KiB, the reset default).
const RTL_RX_SIZE: usize = 8192;
/// Number of rtl8139 transmit descriptors (fixed hardware count).
const RTL_TX_RING: usize = 4;

const E1000_CTRL: u64 = 0x00000;
const E1000_STATUS: u64 = 0x00008;
const E1000_IMC: u64 = 0x000D8;
const E1000_RCTL: u64 = 0x00100;
const E1000_TCTL: u64 = 0x00400;
const E1000_TIPG: u64 = 0x00410;
const E1000_RA: u64 = 0x05400;
const E1000_RDBAL: u64 = 0x02800;
const E1000_RDBAH: u64 = 0x02804;
const E1000_RDLEN: u64 = 0x02808;
const E1000_RDH: u64 = 0x02810;
const E1000_RDT: u64 = 0x02818;
const E1000_TDBAL: u64 = 0x03800;
const E1000_TDBAH: u64 = 0x03804;
const E1000_TDLEN: u64 = 0x03808;
const E1000_TDH: u64 = 0x03810;
const E1000_TDT: u64 = 0x03818;

const E1000_CTRL_FD: u32 = 0x1;
const E1000_CTRL_SLU: u32 = 0x40;
const E1000_STATUS_LU: u32 = 0x2;
const E1000_RCTL_EN: u32 = 0x2;
const E1000_RCTL_BAM: u32 = 0x8000;
const E1000_RCTL_SECRC: u32 = 0x0400_0000;
const E1000_TCTL_EN: u32 = 0x2;
const E1000_TCTL_PSP: u32 = 0x8;
const E1000_TCTL_CT: u32 = 0xff0;
const E1000_TCTL_COLD: u32 = 0x3ff000;
const E1000_RAH_AV: u32 = 0x8000_0000;
const E1000_RXD_STAT_DD: u8 = 0x1;
const E1000_TXD_CMD: u32 = 0x0B00_0000;
const E1000_TXD_STAT_DD: u32 = 0x1;

const RTL_MAC0: u16 = 0x00;
const RTL_TXSTAT0: u16 = 0x10;
const RTL_TXADDR0: u16 = 0x20;
const RTL_RXBUF: u16 = 0x30;
const RTL_CHIPCMD: u16 = 0x37;
const RTL_CAPR: u16 = 0x38;
const RTL_INTRMASK: u16 = 0x3C;
const RTL_TXCFG: u16 = 0x40;
const RTL_RXCFG: u16 = 0x44;
const RTL_BMSR: u16 = 0x64;

const RTL_CMD_RESET: u8 = 0x10;
const RTL_CMD_RXENB: u8 = 0x08;
const RTL_CMD_TXENB: u8 = 0x04;
const RTL_CMD_RX_EMPTY: u8 = 0x1;
const RTL_TX_HOST_OWNS: u32 = 0x2000;
const RTL_TX_OK: u32 = 0x8000;
const RTL_RX_OK: u32 = 0x1;
const RTL_BMSR_LINK: u16 = 0x4;
/// `AcceptMyPhys | AcceptMulticast | AcceptBroadcast | DMA unlimited |
/// FIFO threshold none`, receive ring 8 KiB, hardware wrap (bit 7 clear).
const RTL_RXCFG_VAL: u32 = 0xE70E;
/// `IFG 96 | DMA unlimited` (CRC append stays enabled).
const RTL_TXCFG_VAL: u32 = 0x0300_0700;

/// One e1000 receive descriptor (16 bytes, hardware layout).
#[repr(C)]
#[derive(Clone, Copy)]
struct RxDesc {
    buffer_addr: u64,
    length: u16,
    csum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

impl RxDesc {
    const EMPTY: RxDesc = RxDesc {
        buffer_addr: 0,
        length: 0,
        csum: 0,
        status: 0,
        errors: 0,
        special: 0,
    };
}

/// One e1000 transmit descriptor (16 bytes, legacy layout).
#[repr(C)]
#[derive(Clone, Copy)]
struct TxDesc {
    buffer_addr: u64,
    lower: u32,
    upper: u32,
}

impl TxDesc {
    const EMPTY: TxDesc = TxDesc {
        buffer_addr: 0,
        lower: 0,
        upper: 0,
    };
}

/// Contiguous DMA area for the e1000 rings and packet buffers. Page-aligned
/// so the receive/transmit buffers never straddle a physical discontinuity.
#[repr(C, align(4096))]
struct E1000Dma {
    rx_buf: [[u8; 2048]; E1000_RING],
    tx_buf: [[u8; 2048]; E1000_RING],
    rx_desc: [RxDesc; E1000_RING],
    tx_desc: [TxDesc; E1000_RING],
}

static mut E1000_DMA: E1000Dma = E1000Dma {
    rx_buf: [[0; 2048]; E1000_RING],
    tx_buf: [[0; 2048]; E1000_RING],
    rx_desc: [RxDesc::EMPTY; E1000_RING],
    tx_desc: [TxDesc::EMPTY; E1000_RING],
};

/// Contiguous DMA area for the rtl8139 linear receive buffer and its four
/// transmit buffers.
#[repr(C, align(4096))]
struct Rtl8139Dma {
    rx: [u8; RTL_RX_SIZE],
    tx: [[u8; 2048]; RTL_TX_RING],
}

static mut RTL8139_DMA: Rtl8139Dma = Rtl8139Dma {
    rx: [0; RTL_RX_SIZE],
    tx: [[0; 2048]; RTL_TX_RING],
};

/// Which hardware backend drives this [`Nic`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Intel PRO/1000 (QEMU `-device e1000`).
    E1000,
    /// Realtek 8139 (QEMU `-device rtl8139`).
    Rtl8139,
}

/// A probed and initialised network interface.
pub struct Nic {
    kind: Kind,
    mac: [u8; 6],
    /// e1000 MMIO base (virtual) when [`kind`] is `E1000`.
    mmio: u64,
    /// rtl8139 I/O port base when [`kind`] is `Rtl8139`.
    iobase: u16,
    /// e1000: next ring descriptor to consume / transmit.
    /// rtl8139: receive byte offset (0..8192) / transmit descriptor index.
    rx_pos: u32,
    tx_pos: u32,
}

impl Nic {
    /// Probes the PCI function, enables it and brings the link programming
    /// up to a polling-ready state. `dev` must be a class `0x02` network
    /// controller.
    ///
    /// # Safety
    /// Performs raw MMIO/PIO and DMA setup; call once from boot context.
    pub unsafe fn init(dev: &PciDevice) -> Result<Nic, &'static str> {
        let cmd = pci::config_read32(dev.bus, dev.device, dev.function, 0x04);
        pci::config_write32(dev.bus, dev.device, dev.function, 0x04, cmd | 0x7);
        match dev.vendor_id {
            0x8086 => Nic::init_e1000(dev),
            0x10EC => Nic::init_rtl8139(dev),
            _ => Err("nic: unsupported vendor"),
        }
    }

    /// Hardware MAC address (as read back from the device).
    pub fn mac(&self) -> &[u8; 6] {
        &self.mac
    }

    /// Backend name for GUI/serial status lines.
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            Kind::E1000 => "e1000",
            Kind::Rtl8139 => "rtl8139",
        }
    }

    /// Link status: `STATUS.LU` on e1000, `BMSR` link bit on rtl8139.
    ///
    /// # Safety
    /// Raw MMIO/PIO reads.
    pub unsafe fn link_up(&self) -> bool {
        match self.kind {
            Kind::E1000 => mmio_r32(self.mmio, E1000_STATUS) & E1000_STATUS_LU != 0,
            Kind::Rtl8139 => port::inw(self.iobase + RTL_BMSR) & RTL_BMSR_LINK != 0,
        }
    }

    /// Drains one received frame into `out`, returning its length, or `None`
    /// when the ring holds no complete frame yet.
    ///
    /// # Safety
    /// Reads device-written DMA memory.
    pub unsafe fn poll_rx(&mut self, out: &mut [u8]) -> Option<usize> {
        match self.kind {
            Kind::E1000 => self.poll_rx_e1000(out),
            Kind::Rtl8139 => self.poll_rx_rtl8139(out),
        }
    }

    /// Transmits one Ethernet frame (no FCS), blocking until the device
    /// reports completion or a bounded spin expires.
    ///
    /// # Safety
    /// Programmes device DMA and doorbell registers.
    pub unsafe fn send(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if frame.len() < 14 || frame.len() > FRAME_MAX {
            return Err("nic: bad frame length");
        }
        match self.kind {
            Kind::E1000 => self.send_e1000(frame),
            Kind::Rtl8139 => self.send_rtl8139(frame),
        }
    }

    unsafe fn init_e1000(dev: &PciDevice) -> Result<Nic, &'static str> {
        let bar0 = dev.bars[0];
        if bar0 & 1 != 0 {
            return Err("nic: e1000 bar0 is not memory");
        }
        let low = (bar0 & !0xF) as u64;
        let high = if bar0 & 0b110 == 0b010 {
            (dev.bars[1] as u64) << 32
        } else {
            0
        };
        let mut size = bar_size(dev, 0)? as u64;
        if size == 0 || size > 0x2_0000 {
            size = 0x2_0000;
        }
        let mmio = memory::map_mmio(high | low, size)?;

        mmio_w32(mmio, E1000_IMC, 0xFFFF_FFFF);
        mmio_w32(
            mmio,
            E1000_CTRL,
            mmio_r32(mmio, E1000_CTRL) | E1000_CTRL_SLU | E1000_CTRL_FD,
        );

        let rah = mmio_r32(mmio, E1000_RA + 4);
        let ral = mmio_r32(mmio, E1000_RA);
        let mut mac = [0u8; 6];
        if rah & E1000_RAH_AV != 0 {
            mac[0..4].copy_from_slice(&ral.to_le_bytes());
            let hi = rah & 0xFFFF;
            mac[4] = hi as u8;
            mac[5] = (hi >> 8) as u8;
        } else {
            mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
            mmio_w32(
                mmio,
                E1000_RA,
                u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]),
            );
            mmio_w32(
                mmio,
                E1000_RA + 4,
                u32::from_le_bytes([mac[4], mac[5], 0, 0]) | E1000_RAH_AV,
            );
        }

        let dma = core::ptr::addr_of_mut!(E1000_DMA);
        let rx_buf = ptr::addr_of!((*dma).rx_buf);
        let rx_desc = ptr::addr_of_mut!((*dma).rx_desc).cast::<RxDesc>();
        let tx_desc = ptr::addr_of_mut!((*dma).tx_desc).cast::<TxDesc>();
        for i in 0..E1000_RING {
            let phys = memory::translate(rx_buf as u64 + (i * 2048) as u64)
                .ok_or("nic: e1000 rx buf not mapped")?;
            (*rx_desc.add(i)).buffer_addr = phys;
            (*rx_desc.add(i)).status = 0;
            (*tx_desc.add(i)) = TxDesc::EMPTY;
        }
        let rx_phys = memory::translate(rx_desc as u64).ok_or("nic: e1000 rx ring not mapped")?;
        let tx_phys = memory::translate(tx_desc as u64).ok_or("nic: e1000 tx ring not mapped")?;
        fence(Ordering::Release);

        mmio_w32(mmio, E1000_RDBAH, (rx_phys >> 32) as u32);
        mmio_w32(mmio, E1000_RDBAL, rx_phys as u32 & !0xF);
        mmio_w32(mmio, E1000_RDLEN, (E1000_RING * 16) as u32);
        mmio_w32(mmio, E1000_RDH, 0);
        mmio_w32(mmio, E1000_RDT, (E1000_RING - 1) as u32);

        mmio_w32(mmio, E1000_TDBAH, (tx_phys >> 32) as u32);
        mmio_w32(mmio, E1000_TDBAL, tx_phys as u32 & !0xF);
        mmio_w32(mmio, E1000_TDLEN, (E1000_RING * 16) as u32);
        mmio_w32(mmio, E1000_TDH, 0);
        mmio_w32(mmio, E1000_TDT, 0);

        mmio_w32(
            mmio,
            E1000_TCTL,
            E1000_TCTL_EN | E1000_TCTL_PSP | E1000_TCTL_CT | E1000_TCTL_COLD,
        );
        mmio_w32(mmio, E1000_TIPG, 0x60200A);
        mmio_w32(
            mmio,
            E1000_RCTL,
            E1000_RCTL_EN | E1000_RCTL_BAM | E1000_RCTL_SECRC,
        );

        Ok(Nic {
            kind: Kind::E1000,
            mac,
            mmio,
            iobase: 0,
            rx_pos: 0,
            tx_pos: 0,
        })
    }

    unsafe fn poll_rx_e1000(&mut self, out: &mut [u8]) -> Option<usize> {
        let rdh = mmio_r32(self.mmio, E1000_RDH) as usize % E1000_RING;
        if self.rx_pos as usize == rdh {
            return None;
        }
        let dma = core::ptr::addr_of_mut!(E1000_DMA);
        let idx = self.rx_pos as usize;
        let desc = ptr::addr_of_mut!((*dma).rx_desc).cast::<RxDesc>().add(idx);
        let status = ptr::addr_of!((*desc).status).read_volatile();
        if status & E1000_RXD_STAT_DD == 0 {
            return None;
        }
        let len = ptr::addr_of!((*desc).length).read_volatile() as usize;
        let src = (ptr::addr_of!((*dma).rx_buf) as *const u8).add(idx * 2048);
        let mut taken = 0;
        if len > 0 && len <= out.len() {
            for (i, b) in out[..len].iter_mut().enumerate() {
                *b = src.add(i).read_volatile();
            }
            taken = len;
        }
        let phys = memory::translate(src as u64).unwrap_or(0);
        ptr::addr_of_mut!((*desc).buffer_addr).write_volatile(phys);
        ptr::addr_of_mut!((*desc).length).write_volatile(0);
        ptr::addr_of_mut!((*desc).status).write_volatile(0);
        fence(Ordering::Release);
        mmio_w32(self.mmio, E1000_RDT, self.rx_pos);
        self.rx_pos = ((idx + 1) % E1000_RING) as u32;
        if taken > 0 {
            Some(taken)
        } else {
            self.poll_rx_e1000(out)
        }
    }

    unsafe fn send_e1000(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let t = self.tx_pos as usize;
        let dma = core::ptr::addr_of_mut!(E1000_DMA);
        let dst = (ptr::addr_of_mut!((*dma).tx_buf) as *mut u8).add(t * 2048);
        ptr::copy_nonoverlapping(frame.as_ptr(), dst, frame.len());
        let buf_phys = memory::translate(dst as u64).ok_or("nic: e1000 tx buf not mapped")?;
        let desc = ptr::addr_of_mut!((*dma).tx_desc).cast::<TxDesc>().add(t);
        ptr::addr_of_mut!((*desc).buffer_addr).write_volatile(buf_phys);
        ptr::addr_of_mut!((*desc).lower).write_volatile(frame.len() as u32 | E1000_TXD_CMD);
        ptr::addr_of_mut!((*desc).upper).write_volatile(0);
        fence(Ordering::Release);
        mmio_w32(self.mmio, E1000_TDT, ((t + 1) % E1000_RING) as u32);
        let upper = ptr::addr_of!((*desc).upper);
        for _ in 0..100_000 {
            if upper.read_volatile() & E1000_TXD_STAT_DD != 0 {
                self.tx_pos = ((t + 1) % E1000_RING) as u32;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("nic: e1000 tx timeout")
    }

    unsafe fn init_rtl8139(dev: &PciDevice) -> Result<Nic, &'static str> {
        let bar0 = dev.bars[0];
        if bar0 & 1 == 0 {
            return Err("nic: rtl8139 bar0 is not io");
        }
        let size = bar_size(dev, 0)? as u64;
        if size == 0 || size > 0x100 {
            return Err("nic: rtl8139 io bar too small");
        }
        let iobase = (bar0 & !3) as u16;

        port::outb(iobase + RTL_CHIPCMD, RTL_CMD_RESET);
        port::outb(iobase + RTL_INTRMASK, 0);
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = port::inb(iobase + RTL_MAC0 + i as u16);
        }

        let dma = core::ptr::addr_of_mut!(RTL8139_DMA);
        let rx_phys =
            memory::translate(ptr::addr_of!((*dma).rx) as u64).ok_or("nic: rtl8139 rx buf")?;
        let tx_phys =
            memory::translate(ptr::addr_of!((*dma).tx) as u64).ok_or("nic: rtl8139 tx buf")?;
        for i in 0..RTL_TX_RING {
            port::outl(
                iobase + RTL_TXADDR0 + (i as u16) * 4,
                tx_phys as u32 + (i as u32) * 2048,
            );
        }
        port::outl(iobase + RTL_RXBUF, rx_phys as u32);
        port::outw(iobase + RTL_CAPR, 0u16.wrapping_sub(16));
        port::outl(iobase + RTL_TXCFG, RTL_TXCFG_VAL);
        port::outl(iobase + RTL_RXCFG, RTL_RXCFG_VAL);
        port::outb(iobase + RTL_CHIPCMD, RTL_CMD_RXENB | RTL_CMD_TXENB);
        fence(Ordering::Release);

        Ok(Nic {
            kind: Kind::Rtl8139,
            mac,
            mmio: 0,
            iobase,
            rx_pos: 0,
            tx_pos: 0,
        })
    }

    unsafe fn poll_rx_rtl8139(&mut self, out: &mut [u8]) -> Option<usize> {
        if port::inb(self.iobase + RTL_CHIPCMD) & RTL_CMD_RX_EMPTY != 0 {
            return None;
        }
        let mut hdr = [0u8; 4];
        rtl_copy(self.rx_pos as usize, &mut hdr);
        let word = u32::from_le_bytes(hdr);
        let status = word & 0xFFFF;
        let len_field = ((word >> 16) & 0xFFFF) as usize;
        if len_field > RTL_RX_SIZE {
            return None;
        }
        if status & RTL_RX_OK == 0 || len_field < 4 {
            self.rtl_advance(len_field);
            return None;
        }
        let size = len_field - 4;
        if size == 0 || size > out.len() {
            self.rtl_advance(len_field);
            return None;
        }
        rtl_copy(self.rx_pos as usize + 4, &mut out[..size]);
        self.rtl_advance(len_field);
        Some(size)
    }

    unsafe fn rtl_advance(&mut self, len_field: usize) {
        let next = (self.rx_pos as usize + 4 + len_field + 3) & !3;
        self.rx_pos = (next & (RTL_RX_SIZE - 1)) as u32;
        port::outw(
            self.iobase + RTL_CAPR,
            (self.rx_pos as u16).wrapping_sub(16),
        );
    }

    unsafe fn send_rtl8139(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let t = self.tx_pos as usize;
        let dma = core::ptr::addr_of_mut!(RTL8139_DMA);
        let dst = (ptr::addr_of_mut!((*dma).tx) as *mut u8).add(t * 2048);
        ptr::copy_nonoverlapping(frame.as_ptr(), dst, frame.len());
        let tx_phys = memory::translate(ptr::addr_of!((*dma).tx) as u64)
            .ok_or("nic: rtl8139 tx buf not mapped")?;
        port::outl(
            self.iobase + RTL_TXADDR0 + (t as u16) * 4,
            tx_phys as u32 + (t as u32) * 2048,
        );
        port::outl(
            self.iobase + RTL_TXSTAT0 + (t as u16) * 4,
            frame.len() as u32,
        );
        for _ in 0..100_000 {
            let st = port::inl(self.iobase + RTL_TXSTAT0 + (t as u16) * 4);
            if st & RTL_TX_HOST_OWNS != 0 && st & RTL_TX_OK != 0 {
                self.tx_pos = ((t + 1) % RTL_TX_RING) as u32;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("nic: rtl8139 tx timeout")
    }
}

/// Copies `dst.len()` bytes out of the rtl8139 linear buffer, handling the
/// wrap at the 8 KiB boundary. Reads are volatile because the bytes are
/// written by the device (DMA), not by the CPU.
unsafe fn rtl_copy(offset: usize, dst: &mut [u8]) {
    let dma = core::ptr::addr_of_mut!(RTL8139_DMA);
    let base = ptr::addr_of!((*dma).rx).cast::<u8>();
    let first = (RTL_RX_SIZE - offset).min(dst.len());
    for (i, slot) in dst.iter_mut().enumerate().take(first) {
        *slot = base.add(offset + i).read_volatile();
    }
    for (i, slot) in dst.iter_mut().enumerate().skip(first) {
        *slot = base.add(i - first).read_volatile();
    }
}

/// 32-bit MMIO read at `base + off`.
///
/// # Safety
/// Raw volatile read; `base` must be a mapped device register range.
#[inline]
unsafe fn mmio_r32(base: u64, off: u64) -> u32 {
    ((base + off) as *const u32).read_volatile()
}

/// 32-bit MMIO write at `base + off`.
///
/// # Safety
/// Raw volatile write; `base` must be a mapped device register range.
#[inline]
unsafe fn mmio_w32(base: u64, off: u64, val: u32) {
    ((base + off) as *mut u32).write_volatile(val)
}

/// Sizes a BAR by writing all-ones, reading the mask back and restoring the
/// original value. Returns the decoded size in bytes (0 when unimplemented).
///
/// # Safety
/// Raw PCI configuration I/O.
unsafe fn bar_size(dev: &PciDevice, index: usize) -> Result<u32, &'static str> {
    let offset = 0x10 + (index as u8) * 4;
    let original = pci::config_read32(dev.bus, dev.device, dev.function, offset);
    if original == 0 {
        return Ok(0);
    }
    pci::config_write32(dev.bus, dev.device, dev.function, offset, 0xFFFF_FFFF);
    let mask = pci::config_read32(dev.bus, dev.device, dev.function, offset);
    pci::config_write32(dev.bus, dev.device, dev.function, offset, original);
    if original & 1 != 0 {
        Ok((!(mask & !0x3)).wrapping_add(1) & !0x3)
    } else {
        let size = (!(mask & !0xF)).wrapping_add(1) & !0xF;
        if original & 0b110 == 0b010 {
            let hi_offset = offset + 4;
            let hi_original = pci::config_read32(dev.bus, dev.device, dev.function, hi_offset);
            pci::config_write32(dev.bus, dev.device, dev.function, hi_offset, 0xFFFF_FFFF);
            let hi_mask = pci::config_read32(dev.bus, dev.device, dev.function, hi_offset);
            pci::config_write32(dev.bus, dev.device, dev.function, hi_offset, hi_original);
            if hi_mask != 0 {
                return Ok(u32::MAX);
            }
        }
        Ok(size)
    }
}
