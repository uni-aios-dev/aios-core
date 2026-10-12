//! Bare-metal NIC drivers polled from the idle loop. Six backends are
//! recognised by an exact PCI vendor/device-id table:
//!
//! * Intel PRO/1000 `e1000` family (MMIO descriptor rings),
//! * Intel `e1000e`/`igb`-class parts (reuse the e1000 register core),
//! * Realtek `rtl8139` (PIO linear ring buffer),
//! * Realtek `rtl8169`/`8168` gigabit family (MMIO descriptor rings),
//! * virtio-net legacy (I/O BAR, split virtqueues),
//! * AMD PCnet/PCnet-PCI (I/O, SSIZE32 init block).
//!
//! All run without interrupts — the network stack calls [`Nic::poll_rx`]
//! every tick and [`Nic::send`] blocks bounded.
//!
//! Ring/buffer semantics were derived from the QEMU implementations
//! (`hw/net/e1000.c`, `hw/net/rtl8139.c`, `hw/net/pcnet.c`) and the iPXE
//! `realtek.h` register map, which is the environment the smoke harness
//! runs in:
//!
//! * e1000 RX: descriptors `[RDH .. RDT-1]` are hardware-owned. After
//!   consuming descriptor `k` the driver re-primes it and writes `RDT = k`,
//!   leaving exactly one descriptor of slack.
//! * e1000 TX: descriptor `t` is filled and handed over with `TDT = t+1`;
//!   `cmd = EOP|IFCS|RS` makes the device write `DD` back, which `send`
//!   waits for (bounded).
//! * rtl8139 RX: one 8 KiB linear buffer with hardware wrapping at the end
//!   (`RxConfig` bit 7 clear); `CAPR` is programmed as `read_pos - 16`.
//! * rtl8139 TX: round-robin over four descriptors.
//! * rtl8169 RX/TX: 16-byte descriptors with `OWN` in `opts1`; transmit is
//!   kicked through `TPPoll.NPQ`, receive completions clear `OWN`.
//! * virtio-net: legacy I/O interface (`0x1AF4`), split virtqueues where the
//!   used ring starts at the next 4 KiB boundary after the available ring.
//! * pcnet: I/O interface, `SSIZE32` init block (`BCR20 = 0x0102`) with the
//!   ring length exponents encoded in the high nibbles of `rlen`/`tlen`.

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
/// Number of rtl8169 receive/transmit descriptors.
const RTL8169_RING: usize = 16;
/// Number of virtio-net receive buffers posted at once.
const VIRTIO_RX_BUFS: usize = 16;
/// Bytes reserved per virtio queue region (descriptors + avail + used).
const VIRTIO_VQ_REGION: usize = 16384;
/// Number of pcnet receive/transmit descriptors (`2^3`).
const PCNET_RING: usize = 8;
/// Largest pcnet/rtl8169 per-descriptor packet buffer.
const NIC_BUF: usize = 2048;

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

const R9_MAC: u64 = 0x00;
const R9_MAR0: u64 = 0x08;
const R9_TNPDS: u64 = 0x20;
const R9_CHIPCMD: u64 = 0x37;
const R9_TPPOLL: u64 = 0x38;
const R9_INTRMASK: u64 = 0x3C;
const R9_TXCONFIG: u64 = 0x40;
const R9_RXCONFIG: u64 = 0x44;
const R9_CFG9346: u64 = 0x50;
const R9_PHYSTATUS: u64 = 0x6C;
const R9_RXMAXSIZE: u64 = 0xDA;
const R9_CPLUSCMD: u64 = 0xE0;
const R9_RXDESC: u64 = 0xE4;
const R9_MAXTXPACKET: u64 = 0xEC;

const R9_CMD_RESET: u8 = 0x10;
const R9_CMD_ENB: u8 = 0x0C;
const R9_CFG_UNLOCK: u8 = 0xC0;
const R9_CFG_LOCK: u8 = 0x00;
const R9_POLL_NPQ: u8 = 0x40;
const R9_PHY_LINK: u8 = 0x02;
const R9_DESC_OWN: u32 = 0x8000_0000;
const R9_DESC_EOR: u32 = 0x4000_0000;
const R9_DESC_FS: u32 = 0x2000_0000;
const R9_DESC_LS: u32 = 0x1000_0000;
const R9_RX_LEN_MASK: u32 = 0x3FFF;
const R9_RXCONFIG_VAL: u32 = 0xE70E;
const R9_TXCONFIG_VAL: u32 = 0x0300_0700;

const VIRTIO_STATUS: u16 = 0x12;
const VIRTIO_QUEUE_SEL: u16 = 0x0E;
const VIRTIO_QUEUE_NUM: u16 = 0x0C;
const VIRTIO_QUEUE_PFN: u16 = 0x08;
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_GUEST_FEATURES: u16 = 0x04;
const VIRTIO_HOST_FEATURES: u16 = 0x00;
const VIRTIO_CONFIG: u16 = 0x14;
const VIRTIO_STATUS_ACK: u8 = 0x1;
const VIRTIO_STATUS_DRIVER: u8 = 0x2;
const VIRTIO_STATUS_FEAT_OK: u8 = 0x8;
const VIRTIO_NET_F_MAC: u32 = 0x20;
const VIRTQ_DESC_F_WRITE: u16 = 0x2;

const PCNET_RDP: u16 = 0x10;
const PCNET_RAP: u16 = 0x12;
const PCNET_RESET: u16 = 0x14;
const PCNET_BDP: u16 = 0x16;
/// Reset port in the 32-bit (`DWIO`) register window; reading it is harmless
/// when the controller runs in the default 16-bit window.
const PCNET_DWIO_RESET: u16 = 0x18;
const PCNET_CSR0: u16 = 0x00;
const PCNET_CSR1: u16 = 0x01;
const PCNET_CSR2: u16 = 0x02;
const PCNET_BCR20: u16 = 20;
const PCNET_CSR0_INIT: u16 = 0x0001;
const PCNET_CSR0_STRT: u16 = 0x0002;
const PCNET_CSR0_TDMD: u16 = 0x0008;
const PCNET_CSR0_IDON: u16 = 0x0100;
const PCNET_SWSTYLE32: u16 = 0x0102;
const PCNET_RX_OWN: u16 = 0x8000;
const PCNET_TX_OWN: u16 = 0x8000;
const PCNET_TX_STP_ENP: u16 = 0x0300;
const PCNET_RX_LEN_EXP: u8 = 3;
const PCNET_TX_LEN_EXP: u8 = 3;

const INTEL: u16 = 0x8086;
const REALTEK: u16 = 0x10EC;
const AMD: u16 = 0x1022;
const VIRTIO: u16 = 0x1AF4;

/// Legacy Intel PRO/1000 device ids (82540/82541/82545/82546/82547 family).
const E1000_IDS: &[u16] = &[
    0x1000, 0x1001, 0x1004, 0x1008, 0x1009, 0x100C, 0x100D, 0x100E, 0x100F, 0x1010, 0x1011, 0x1012,
    0x1013, 0x1014, 0x1015, 0x1016, 0x1017, 0x1018, 0x1019, 0x101A, 0x101D, 0x101E, 0x1026, 0x1027,
    0x1028, 0x105E, 0x105F, 0x1060, 0x1075, 0x1076, 0x1077, 0x1078, 0x1079, 0x107A, 0x107B, 0x107C,
    0x108A, 0x1099, 0x10B5, 0x10D5, 0x10D9, 0x10DA, 0x10DE, 0x10DF, 0x1501,
];

/// Intel e1000e/igb-class device ids (82571..82580, ICH8/9/10, PCIm).
const E1000E_IDS: &[u16] = &[
    0x1049, 0x104A, 0x104B, 0x104C, 0x104D, 0x10A4, 0x10A5, 0x10A7, 0x10A9, 0x10AE, 0x10B9, 0x10BA,
    0x10BB, 0x10BC, 0x10BD, 0x10BE, 0x10BF, 0x10C0, 0x10C2, 0x10C3, 0x10C4, 0x10C5, 0x10CB, 0x10CC,
    0x10CD, 0x10CE, 0x10D3, 0x10D5, 0x10D9, 0x10E5, 0x10EA, 0x10EB, 0x10EF, 0x10F0, 0x10F6, 0x1502,
    0x1503, 0x150A, 0x150C, 0x150D, 0x150E, 0x150F, 0x1510, 0x1511, 0x1516, 0x1518, 0x1521, 0x1522,
    0x1523, 0x1524, 0x1559, 0x155A, 0x294C, 0x2E6E,
];

/// Realtek RTL8139/8138 device ids.
const RTL8139_IDS: &[u16] = &[0x8138, 0x8139];
/// Realtek RTL8169/8168/810x gigabit device ids.
const RTL8169_IDS: &[u16] = &[
    0x8110, 0x8111, 0x8161, 0x8167, 0x8168, 0x8169, 0x8136, 0x8101,
];

/// AMD PCnet/PCnet-PCI device ids.
const PCNET_IDS: &[u16] = &[0x2000, 0x2001];

/// Which hardware backend drives this [`Nic`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Intel PRO/1000 (QEMU `-device e1000`).
    E1000,
    /// Intel e1000e/igb-class parts driven through the e1000 register core.
    E1000E,
    /// Realtek 8139/8138 (QEMU `-device rtl8139`).
    Rtl8139,
    /// Realtek 8169/8168/810x gigabit family.
    Rtl8169,
    /// virtio-net legacy (`0x1AF4:0x1000`).
    VirtioNet,
    /// AMD PCnet/PCnet-PCI (QEMU `-device pcnet`).
    Pcnet,
}

/// Resolves a network PCI function to a supported [`Kind`] using an exact
/// vendor/device-id table (not vendor alone), or `None` for unknown parts.
pub fn driver_kind(dev: &PciDevice) -> Option<Kind> {
    let id = dev.device_id;
    match dev.vendor_id {
        INTEL => {
            if E1000_IDS.contains(&id) {
                Some(Kind::E1000)
            } else if E1000E_IDS.contains(&id) {
                Some(Kind::E1000E)
            } else {
                None
            }
        }
        REALTEK => {
            if RTL8139_IDS.contains(&id) {
                Some(Kind::Rtl8139)
            } else if RTL8169_IDS.contains(&id) {
                Some(Kind::Rtl8169)
            } else {
                None
            }
        }
        VIRTIO => {
            if id == 0x1000 || id == 0x1001 {
                Some(Kind::VirtioNet)
            } else {
                None
            }
        }
        AMD => {
            if PCNET_IDS.contains(&id) {
                Some(Kind::Pcnet)
            } else {
                None
            }
        }
        _ => None,
    }
}

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
    rx_buf: [[u8; NIC_BUF]; E1000_RING],
    tx_buf: [[u8; NIC_BUF]; E1000_RING],
    rx_desc: [RxDesc; E1000_RING],
    tx_desc: [TxDesc; E1000_RING],
}

static mut E1000_DMA: E1000Dma = E1000Dma {
    rx_buf: [[0; NIC_BUF]; E1000_RING],
    tx_buf: [[0; NIC_BUF]; E1000_RING],
    rx_desc: [RxDesc::EMPTY; E1000_RING],
    tx_desc: [TxDesc::EMPTY; E1000_RING],
};

/// Contiguous DMA area for the rtl8139 linear receive buffer and its four
/// transmit buffers.
#[repr(C, align(4096))]
struct Rtl8139Dma {
    rx: [u8; RTL_RX_SIZE],
    tx: [[u8; NIC_BUF]; RTL_TX_RING],
}

static mut RTL8139_DMA: Rtl8139Dma = Rtl8139Dma {
    rx: [0; RTL_RX_SIZE],
    tx: [[0; NIC_BUF]; RTL_TX_RING],
};

/// Contiguous DMA area for the rtl8169 descriptor rings and buffers. The
/// 256-byte alignment keeps both descriptor rings on their required boundary.
#[repr(C, align(256))]
struct Rtl8169Dma {
    rx_desc: [[u8; 16]; RTL8169_RING],
    tx_desc: [[u8; 16]; RTL8169_RING],
    rx_buf: [[u8; NIC_BUF]; RTL8169_RING],
    tx_buf: [[u8; NIC_BUF]; RTL8169_RING],
}

static mut RTL8169_DMA: Rtl8169Dma = Rtl8169Dma {
    rx_desc: [[0; 16]; RTL8169_RING],
    tx_desc: [[0; 16]; RTL8169_RING],
    rx_buf: [[0; NIC_BUF]; RTL8169_RING],
    tx_buf: [[0; NIC_BUF]; RTL8169_RING],
};

/// Contiguous DMA area for the two virtio-net split virtqueues (0 = receive,
/// 1 = transmit) plus their packet buffers. Each queue region is 4 KiB aligned
/// through the outer alignment.
#[repr(C, align(4096))]
struct VirtioDma {
    q: [[u8; VIRTIO_VQ_REGION]; 2],
    rx_buf: [[u8; NIC_BUF]; VIRTIO_RX_BUFS],
    tx_buf: [u8; NIC_BUF],
}

static mut VIRTIO_DMA: VirtioDma = VirtioDma {
    q: [[0; VIRTIO_VQ_REGION]; 2],
    rx_buf: [[0; NIC_BUF]; VIRTIO_RX_BUFS],
    tx_buf: [0; NIC_BUF],
};

/// Contiguous DMA area for the pcnet init block, descriptor rings and buffers.
#[repr(C, align(16))]
struct PcnetDma {
    init: [u8; 32],
    rx_desc: [[u8; 16]; PCNET_RING],
    tx_desc: [[u8; 16]; PCNET_RING],
    rx_buf: [[u8; NIC_BUF]; PCNET_RING],
    tx_buf: [[u8; NIC_BUF]; PCNET_RING],
}

static mut PCNET_DMA: PcnetDma = PcnetDma {
    init: [0; 32],
    rx_desc: [[0; 16]; PCNET_RING],
    tx_desc: [[0; 16]; PCNET_RING],
    rx_buf: [[0; NIC_BUF]; PCNET_RING],
    tx_buf: [[0; NIC_BUF]; PCNET_RING],
};

/// A probed and initialised network interface.
pub struct Nic {
    kind: Kind,
    mac: [u8; 6],
    /// e1000/e1000e/rtl8169 MMIO base (virtual) when applicable.
    mmio: u64,
    /// rtl8139/virtio-net/pcnet I/O port base when applicable.
    iobase: u16,
    /// virtio: negotiated queue size for both queues (0 otherwise).
    qsize: u16,
    /// e1000/e1000e/rtl8169/pcnet: next ring descriptor to consume.
    /// rtl8139: receive byte offset (0..8192).
    /// virtio: consumed receive used-ring index.
    rx_pos: u32,
    /// e1000/e1000e/rtl8169: next transmit descriptor.
    /// rtl8139/virtio-net/pcnet: transmit cursor.
    tx_pos: u32,
}

impl Nic {
    /// Probes the PCI function, enables it and brings the link programming
    /// up to a polling-ready state. `dev` must be a class `0x02` network
    /// controller; unrecognised vendor/device pairs are rejected explicitly.
    ///
    /// # Safety
    /// Performs raw MMIO/PIO and DMA setup; call once from boot context.
    pub unsafe fn init(dev: &PciDevice) -> Result<Nic, &'static str> {
        let cmd = pci::config_read32(dev.bus, dev.device, dev.function, 0x04);
        pci::config_write32(dev.bus, dev.device, dev.function, 0x04, cmd | 0x7);
        let kind = driver_kind(dev).ok_or("nic: unsupported device")?;
        match kind {
            Kind::E1000 => Nic::init_e1000(dev, Kind::E1000),
            Kind::E1000E => Nic::init_e1000(dev, Kind::E1000E),
            Kind::Rtl8139 => Nic::init_rtl8139(dev),
            Kind::Rtl8169 => Nic::init_rtl8169(dev),
            Kind::VirtioNet => Nic::init_virtio(dev),
            Kind::Pcnet => Nic::init_pcnet(dev),
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
            Kind::E1000E => "e1000e",
            Kind::Rtl8139 => "rtl8139",
            Kind::Rtl8169 => "rtl8169",
            Kind::VirtioNet => "virtio-net",
            Kind::Pcnet => "pcnet",
        }
    }

    /// Link status: `STATUS.LU` on e1000/e1000e, `BMSR` link bit on rtl8139,
    /// `PHYstatus` link bit on rtl8169. virtio-net and pcnet report the link
    /// as up (their emulated PHYs never drop in the polling model).
    ///
    /// # Safety
    /// Raw MMIO/PIO reads.
    pub unsafe fn link_up(&self) -> bool {
        match self.kind {
            Kind::E1000 | Kind::E1000E => mmio_r32(self.mmio, E1000_STATUS) & E1000_STATUS_LU != 0,
            Kind::Rtl8139 => port::inw(self.iobase + RTL_BMSR) & RTL_BMSR_LINK != 0,
            Kind::Rtl8169 => mmio_r8(self.mmio, R9_PHYSTATUS) & R9_PHY_LINK != 0,
            Kind::VirtioNet | Kind::Pcnet => true,
        }
    }

    /// Drains one received frame into `out`, returning its length, or `None`
    /// when the ring holds no complete frame yet.
    ///
    /// # Safety
    /// Reads device-written DMA memory.
    pub unsafe fn poll_rx(&mut self, out: &mut [u8]) -> Option<usize> {
        match self.kind {
            Kind::E1000 | Kind::E1000E => self.poll_rx_e1000(out),
            Kind::Rtl8139 => self.poll_rx_rtl8139(out),
            Kind::Rtl8169 => self.poll_rx_rtl8169(out),
            Kind::VirtioNet => self.poll_rx_virtio(out),
            Kind::Pcnet => self.poll_rx_pcnet(out),
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
            Kind::E1000 | Kind::E1000E => self.send_e1000(frame),
            Kind::Rtl8139 => self.send_rtl8139(frame),
            Kind::Rtl8169 => self.send_rtl8169(frame),
            Kind::VirtioNet => self.send_virtio(frame),
            Kind::Pcnet => self.send_pcnet(frame),
        }
    }

    unsafe fn init_e1000(dev: &PciDevice, kind: Kind) -> Result<Nic, &'static str> {
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
            let phys = memory::translate(rx_buf as u64 + (i * NIC_BUF) as u64)
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
            kind,
            mac,
            mmio,
            iobase: 0,
            qsize: 0,
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
        let src = (ptr::addr_of!((*dma).rx_buf) as *const u8).add(idx * NIC_BUF);
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
        let dst = (ptr::addr_of_mut!((*dma).tx_buf) as *mut u8).add(t * NIC_BUF);
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
                tx_phys as u32 + (i as u32) * NIC_BUF as u32,
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
            qsize: 0,
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
        let dst = (ptr::addr_of_mut!((*dma).tx) as *mut u8).add(t * NIC_BUF);
        ptr::copy_nonoverlapping(frame.as_ptr(), dst, frame.len());
        let tx_phys = memory::translate(ptr::addr_of!((*dma).tx) as u64)
            .ok_or("nic: rtl8139 tx buf not mapped")?;
        port::outl(
            self.iobase + RTL_TXADDR0 + (t as u16) * 4,
            tx_phys as u32 + (t as u32) * NIC_BUF as u32,
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

    unsafe fn init_rtl8169(dev: &PciDevice) -> Result<Nic, &'static str> {
        let (bar, index) = pick_memory_bar(dev).ok_or("nic: rtl8169 has no memory bar")?;
        let low = (bar & !0xF) as u64;
        let high = if bar & 0b110 == 0b010 && index + 1 < dev.bars.len() {
            (dev.bars[index + 1] as u64) << 32
        } else {
            0
        };
        let mut size = bar_size(dev, index)? as u64;
        if size == 0 || size > 0x1_0000 {
            size = 0x1_0000;
        }
        let mmio = memory::map_mmio(high | low, size)?;

        mmio_w8(mmio, R9_CHIPCMD, R9_CMD_RESET);
        for _ in 0..100_000 {
            if mmio_r8(mmio, R9_CHIPCMD) & R9_CMD_RESET == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        mmio_w8(mmio, R9_CFG9346, R9_CFG_UNLOCK);
        mmio_w16(mmio, R9_CPLUSCMD, 0);
        mmio_w16(mmio, R9_RXMAXSIZE, 1536);
        mmio_w8(mmio, R9_MAXTXPACKET, (NIC_BUF / 128) as u8);

        let dma = core::ptr::addr_of_mut!(RTL8169_DMA);
        let rx_desc = (ptr::addr_of_mut!((*dma).rx_desc) as *mut u8).cast::<u8>();
        let tx_desc = (ptr::addr_of_mut!((*dma).tx_desc) as *mut u8).cast::<u8>();
        let rx_buf = (ptr::addr_of_mut!((*dma).rx_buf) as *mut u8).cast::<u8>();
        let tx_buf = (ptr::addr_of_mut!((*dma).tx_buf) as *mut u8).cast::<u8>();
        for i in 0..RTL8169_RING {
            let buf_phys = memory::translate(rx_buf as u64 + (i * NIC_BUF) as u64)
                .ok_or("nic: rtl8169 rx buf not mapped")?;
            let d = rx_desc.add(i * 16);
            let eor = if i == RTL8169_RING - 1 {
                R9_DESC_EOR
            } else {
                0
            };
            wr32(d, R9_DESC_OWN | eor | NIC_BUF as u32);
            wr32(d.add(4), 0);
            wr64(d.add(8), buf_phys);

            let tb_phys = memory::translate(tx_buf as u64 + (i * NIC_BUF) as u64)
                .ok_or("nic: rtl8169 tx buf not mapped")?;
            let td = tx_desc.add(i * 16);
            let teor = if i == RTL8169_RING - 1 {
                R9_DESC_EOR
            } else {
                0
            };
            wr32(td, teor);
            wr32(td.add(4), 0);
            wr64(td.add(8), tb_phys);
        }
        let rx_phys = memory::translate(rx_desc as u64).ok_or("nic: rtl8169 rx ring not mapped")?;
        let tx_phys = memory::translate(tx_desc as u64).ok_or("nic: rtl8169 tx ring not mapped")?;
        fence(Ordering::Release);

        mmio_w32(mmio, R9_TNPDS, tx_phys as u32);
        mmio_w32(mmio, R9_TNPDS + 4, (tx_phys >> 32) as u32);
        mmio_w32(mmio, R9_RXDESC, rx_phys as u32);
        mmio_w32(mmio, R9_RXDESC + 4, (rx_phys >> 32) as u32);
        mmio_w32(mmio, R9_MAR0, 0xFFFF_FFFF);
        mmio_w32(mmio, R9_MAR0 + 4, 0xFFFF_FFFF);
        mmio_w8(mmio, R9_CHIPCMD, R9_CMD_ENB);
        mmio_w32(mmio, R9_TXCONFIG, R9_TXCONFIG_VAL);
        mmio_w32(mmio, R9_RXCONFIG, R9_RXCONFIG_VAL);
        mmio_w16(mmio, R9_INTRMASK, 0);
        mmio_w8(mmio, R9_CFG9346, R9_CFG_LOCK);

        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = mmio_r8(mmio, R9_MAC + i as u64);
        }
        if mac == [0u8; 6] {
            mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        }

        Ok(Nic {
            kind: Kind::Rtl8169,
            mac,
            mmio,
            iobase: 0,
            qsize: 0,
            rx_pos: 0,
            tx_pos: 0,
        })
    }

    unsafe fn poll_rx_rtl8169(&mut self, out: &mut [u8]) -> Option<usize> {
        let dma = core::ptr::addr_of_mut!(RTL8169_DMA);
        let rx_desc = (ptr::addr_of_mut!((*dma).rx_desc) as *mut u8).cast::<u8>();
        let cur = self.rx_pos as usize;
        let d = rx_desc.add(cur * 16);
        let opts1 = rd32(d);
        if opts1 & R9_DESC_OWN != 0 {
            return None;
        }
        let len = (opts1 & R9_RX_LEN_MASK) as usize;
        let src = (ptr::addr_of!((*dma).rx_buf) as *const u8).add(cur * NIC_BUF);
        let mut taken = 0;
        if len >= 4 {
            let n = (len - 4).min(out.len());
            for (i, b) in out[..n].iter_mut().enumerate() {
                *b = src.add(i).read_volatile();
            }
            taken = n;
        }
        let eor = if cur == RTL8169_RING - 1 {
            R9_DESC_EOR
        } else {
            0
        };
        wr32(d, R9_DESC_OWN | eor | NIC_BUF as u32);
        wr32(d.add(4), 0);
        fence(Ordering::Release);
        self.rx_pos = ((cur + 1) % RTL8169_RING) as u32;
        if taken > 0 {
            Some(taken)
        } else {
            self.poll_rx_rtl8169(out)
        }
    }

    unsafe fn send_rtl8169(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let t = self.tx_pos as usize;
        let dma = core::ptr::addr_of_mut!(RTL8169_DMA);
        let tx_desc = (ptr::addr_of_mut!((*dma).tx_desc) as *mut u8).cast::<u8>();
        let d = tx_desc.add(t * 16);
        if rd32(d) & R9_DESC_OWN != 0 {
            return Err("nic: rtl8169 tx busy");
        }
        let dst = (ptr::addr_of_mut!((*dma).tx_buf) as *mut u8).add(t * NIC_BUF);
        ptr::copy_nonoverlapping(frame.as_ptr(), dst, frame.len());
        let buf_phys = memory::translate(dst as u64).ok_or("nic: rtl8169 tx buf not mapped")?;
        wr64(d.add(8), buf_phys);
        wr32(d.add(4), 0);
        let eor = if t == RTL8169_RING - 1 {
            R9_DESC_EOR
        } else {
            0
        };
        wr32(
            d,
            R9_DESC_OWN | eor | R9_DESC_FS | R9_DESC_LS | frame.len() as u32,
        );
        fence(Ordering::Release);
        mmio_w8(self.mmio, R9_TPPOLL, R9_POLL_NPQ);
        for _ in 0..100_000 {
            if rd32(d) & R9_DESC_OWN == 0 {
                self.tx_pos = ((t + 1) % RTL8169_RING) as u32;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("nic: rtl8169 tx timeout")
    }

    unsafe fn init_virtio(dev: &PciDevice) -> Result<Nic, &'static str> {
        let bar0 = dev.bars[0];
        if bar0 & 1 == 0 {
            return Err("nic: virtio modern memory bar unsupported");
        }
        let iobase = (bar0 & !3) as u16;

        port::outb(iobase + VIRTIO_STATUS, 0);
        port::outb(iobase + VIRTIO_STATUS, VIRTIO_STATUS_ACK);
        port::outb(
            iobase + VIRTIO_STATUS,
            VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER,
        );
        let host = port::inl(iobase + VIRTIO_HOST_FEATURES);
        port::outl(iobase + VIRTIO_GUEST_FEATURES, host & VIRTIO_NET_F_MAC);
        port::outb(
            iobase + VIRTIO_STATUS,
            VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEAT_OK,
        );
        if port::inb(iobase + VIRTIO_STATUS) & VIRTIO_STATUS_FEAT_OK == 0 {
            return Err("nic: virtio feature negotiation failed");
        }

        let dma = core::ptr::addr_of_mut!(VIRTIO_DMA);
        let q0 = (ptr::addr_of_mut!((*dma).q) as *mut u8).cast::<u8>();
        let q1 = q0.add(VIRTIO_VQ_REGION);
        let rx_buf = (ptr::addr_of_mut!((*dma).rx_buf) as *mut u8).cast::<u8>();

        let mut qsize = 0u16;
        for (q, region) in [(0u16, q0), (1u16, q1)] {
            port::outw(iobase + VIRTIO_QUEUE_SEL, q);
            let size = port::inw(iobase + VIRTIO_QUEUE_NUM);
            if size == 0 {
                return Err("nic: virtio queue not present");
            }
            let used_off = virtio_used_off(size as usize);
            let need = used_off + 6 + 8 * size as usize;
            if need > VIRTIO_VQ_REGION {
                return Err("nic: virtio queue region too small");
            }
            qsize = size;
            let region_phys =
                memory::translate(region as u64).ok_or("nic: virtio queue not mapped")?;
            port::outl(iobase + VIRTIO_QUEUE_PFN, (region_phys >> 12) as u32);
        }

        let avail_off0 = virtio_avail_off(qsize as usize);
        let bufs = VIRTIO_RX_BUFS.min(qsize as usize);
        for i in 0..bufs {
            let bp = memory::translate(rx_buf as u64 + (i * NIC_BUF) as u64)
                .ok_or("nic: virtio rx buf not mapped")?;
            let d = q0.add(i * 16);
            wr64(d, bp);
            wr32(d.add(8), NIC_BUF as u32);
            wr16(d.add(12), VIRTQ_DESC_F_WRITE);
            wr16(d.add(14), 0);
            wr16(q0.add(avail_off0 + 4 + i * 2), i as u16);
        }
        wr16(q0.add(avail_off0 + 2), bufs as u16);
        fence(Ordering::Release);
        port::outw(iobase + VIRTIO_QUEUE_NOTIFY, 0);

        let mut mac = [0u8; 6];
        if host & VIRTIO_NET_F_MAC != 0 {
            for (i, b) in mac.iter_mut().enumerate() {
                *b = port::inb(iobase + VIRTIO_CONFIG + i as u16);
            }
        }
        if mac == [0u8; 6] {
            mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        }

        Ok(Nic {
            kind: Kind::VirtioNet,
            mac,
            mmio: 0,
            iobase,
            qsize,
            rx_pos: 0,
            tx_pos: 0,
        })
    }

    unsafe fn poll_rx_virtio(&mut self, out: &mut [u8]) -> Option<usize> {
        let dma = core::ptr::addr_of_mut!(VIRTIO_DMA);
        let q0 = (ptr::addr_of_mut!((*dma).q) as *mut u8).cast::<u8>();
        let rx_buf = (ptr::addr_of_mut!((*dma).rx_buf) as *mut u8).cast::<u8>();
        let qsize = self.qsize as usize;
        let avail_off = virtio_avail_off(qsize);
        let used_off = virtio_used_off(qsize);
        let used_idx = rd16(q0.add(used_off + 2));
        if (self.rx_pos as u16) == used_idx {
            return None;
        }
        let slot = (self.rx_pos as usize) % qsize;
        let id = rd32(q0.add(used_off + 4 + slot * 8)) as usize;
        let len = rd32(q0.add(used_off + 4 + slot * 8 + 4)) as usize;
        let n = len.min(NIC_BUF).min(out.len());
        let src = rx_buf.add(id * NIC_BUF);
        for (i, b) in out[..n].iter_mut().enumerate() {
            *b = src.add(i).read_volatile();
        }
        let bp = memory::translate(src as u64).unwrap_or(0);
        let d = q0.add(id * 16);
        wr64(d, bp);
        wr32(d.add(8), NIC_BUF as u32);
        wr16(d.add(12), VIRTQ_DESC_F_WRITE);
        wr16(d.add(14), 0);
        let aidx = rd16(q0.add(avail_off + 2));
        wr16(
            q0.add(avail_off + 4 + (aidx as usize % qsize) * 2),
            id as u16,
        );
        wr16(q0.add(avail_off + 2), aidx.wrapping_add(1));
        fence(Ordering::Release);
        port::outw(self.iobase + VIRTIO_QUEUE_NOTIFY, 0);
        self.rx_pos = self.rx_pos.wrapping_add(1);
        if n > 0 {
            Some(n)
        } else {
            self.poll_rx_virtio(out)
        }
    }

    unsafe fn send_virtio(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let dma = core::ptr::addr_of_mut!(VIRTIO_DMA);
        let q1 = ((ptr::addr_of_mut!((*dma).q) as *mut u8).cast::<u8>()).add(VIRTIO_VQ_REGION);
        let tx_buf = (ptr::addr_of_mut!((*dma).tx_buf) as *mut u8).cast::<u8>();
        let qsize = self.qsize as usize;
        let avail_off = virtio_avail_off(qsize);
        let used_off = virtio_used_off(qsize);
        ptr::copy_nonoverlapping(frame.as_ptr(), tx_buf, frame.len());
        let bp = memory::translate(tx_buf as u64).ok_or("nic: virtio tx buf not mapped")?;
        wr64(q1, bp);
        wr32(q1.add(8), frame.len() as u32);
        wr16(q1.add(12), 0);
        wr16(q1.add(14), 0);
        let aidx = rd16(q1.add(avail_off + 2));
        wr16(q1.add(avail_off + 4 + (aidx as usize % qsize) * 2), 0);
        wr16(q1.add(avail_off + 2), aidx.wrapping_add(1));
        fence(Ordering::Release);
        port::outw(self.iobase + VIRTIO_QUEUE_NOTIFY, 1);
        let want = self.tx_pos.wrapping_add(1) as u16;
        for _ in 0..100_000 {
            if rd16(q1.add(used_off + 2)) == want {
                self.tx_pos = want as u32;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("nic: virtio tx timeout")
    }

    unsafe fn init_pcnet(dev: &PciDevice) -> Result<Nic, &'static str> {
        let bar0 = dev.bars[0];
        if bar0 & 1 == 0 {
            return Err("nic: pcnet bar0 is not io");
        }
        let iobase = (bar0 & !3) as u16;

        let _ = port::inl(iobase + PCNET_DWIO_RESET);
        let _ = port::inw(iobase + PCNET_RESET);
        crate::interrupts::delay_ms(10);

        pcnet_write_bcr(iobase, PCNET_BCR20, PCNET_SWSTYLE32);
        pcnet_write_csr(iobase, PCNET_CSR0, PCNET_CSR0_INIT);

        let dma = core::ptr::addr_of_mut!(PCNET_DMA);
        let init = (ptr::addr_of_mut!((*dma).init) as *mut u8).cast::<u8>();
        let rx_desc = (ptr::addr_of_mut!((*dma).rx_desc) as *mut u8).cast::<u8>();
        let tx_desc = (ptr::addr_of_mut!((*dma).tx_desc) as *mut u8).cast::<u8>();
        let rx_buf = (ptr::addr_of_mut!((*dma).rx_buf) as *mut u8).cast::<u8>();
        let tx_buf = (ptr::addr_of_mut!((*dma).tx_buf) as *mut u8).cast::<u8>();

        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = port::inb(iobase + i as u16);
        }
        if mac == [0u8; 6] {
            mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        }

        for i in 0..PCNET_RING {
            let bp = memory::translate(rx_buf as u64 + (i * NIC_BUF) as u64)
                .ok_or("nic: pcnet rx buf not mapped")?;
            let d = rx_desc.add(i * 16);
            wr32(d, bp as u32);
            wr16(d.add(4), 0xF800);
            wr16(d.add(6), PCNET_RX_OWN);
            wr32(d.add(8), 0);
            wr32(d.add(12), 0);

            let tb = memory::translate(tx_buf as u64 + (i * NIC_BUF) as u64)
                .ok_or("nic: pcnet tx buf not mapped")?;
            let td = tx_desc.add(i * 16);
            wr32(td, tb as u32);
            wr16(td.add(4), 0);
            wr16(td.add(6), 0);
            wr32(td.add(8), 0);
            wr32(td.add(12), 0);
        }
        let rx_phys = memory::translate(rx_desc as u64).ok_or("nic: pcnet rx ring not mapped")?;
        let tx_phys = memory::translate(tx_desc as u64).ok_or("nic: pcnet tx ring not mapped")?;

        for i in 0..32 {
            init.add(i).write_volatile(0);
        }
        wr16(init, 0);
        *init.add(2) = PCNET_RX_LEN_EXP << 4;
        *init.add(3) = PCNET_TX_LEN_EXP << 4;
        for (i, b) in mac.iter().enumerate() {
            *init.add(4 + i) = *b;
        }
        wr32(init.add(20), rx_phys as u32);
        wr32(init.add(24), tx_phys as u32);
        fence(Ordering::Release);

        let init_phys = memory::translate(init as u64).ok_or("nic: pcnet init not mapped")?;
        pcnet_write_csr(iobase, PCNET_CSR1, (init_phys & 0xFFFF) as u16);
        pcnet_write_csr(iobase, PCNET_CSR2, ((init_phys >> 16) & 0xFFFF) as u16);
        pcnet_write_csr(iobase, PCNET_CSR0, PCNET_CSR0_INIT);
        let mut ok = false;
        for _ in 0..1_000_000 {
            if pcnet_read_csr(iobase, PCNET_CSR0) & PCNET_CSR0_IDON != 0 {
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !ok {
            return Err("nic: pcnet init timed out");
        }
        pcnet_write_csr(iobase, PCNET_CSR0, PCNET_CSR0_STRT);

        Ok(Nic {
            kind: Kind::Pcnet,
            mac,
            mmio: 0,
            iobase,
            qsize: 0,
            rx_pos: 0,
            tx_pos: 0,
        })
    }

    unsafe fn poll_rx_pcnet(&mut self, out: &mut [u8]) -> Option<usize> {
        let dma = core::ptr::addr_of_mut!(PCNET_DMA);
        let rx_desc = (ptr::addr_of_mut!((*dma).rx_desc) as *mut u8).cast::<u8>();
        let rx_buf = (ptr::addr_of_mut!((*dma).rx_buf) as *mut u8).cast::<u8>();
        let cur = self.rx_pos as usize;
        let d = rx_desc.add(cur * 16);
        if rd16(d.add(6)) & PCNET_RX_OWN != 0 {
            return None;
        }
        let mlen = (rd32(d.add(8)) & 0x0FFF) as usize;
        let mut taken = 0;
        if mlen > 4 {
            let n = (mlen - 4).min(out.len());
            let src = rx_buf.add(cur * NIC_BUF);
            for (i, b) in out[..n].iter_mut().enumerate() {
                *b = src.add(i).read_volatile();
            }
            taken = n;
        }
        wr32(d.add(8), 0);
        wr16(d.add(4), 0xF800);
        wr16(d.add(6), PCNET_RX_OWN);
        fence(Ordering::Release);
        self.rx_pos = ((cur + 1) % PCNET_RING) as u32;
        if taken > 0 {
            Some(taken)
        } else {
            self.poll_rx_pcnet(out)
        }
    }

    unsafe fn send_pcnet(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let dma = core::ptr::addr_of_mut!(PCNET_DMA);
        let tx_desc = (ptr::addr_of_mut!((*dma).tx_desc) as *mut u8).cast::<u8>();
        let tx_buf = (ptr::addr_of_mut!((*dma).tx_buf) as *mut u8).cast::<u8>();
        let t = self.tx_pos as usize;
        let d = tx_desc.add(t * 16);
        if rd16(d.add(6)) & PCNET_TX_OWN != 0 {
            return Err("nic: pcnet tx busy");
        }
        let dst = tx_buf.add(t * NIC_BUF);
        ptr::copy_nonoverlapping(frame.as_ptr(), dst, frame.len());
        let bp = memory::translate(dst as u64).ok_or("nic: pcnet tx buf not mapped")?;
        wr32(d, bp as u32);
        wr16(d.add(4), (-(frame.len() as i32)) as u16);
        wr32(d.add(8), 0);
        wr32(d.add(12), 0);
        fence(Ordering::Release);
        wr16(d.add(6), PCNET_TX_OWN | PCNET_TX_STP_ENP);
        pcnet_write_csr(self.iobase, PCNET_CSR0, PCNET_CSR0_STRT | PCNET_CSR0_TDMD);
        for _ in 0..100_000 {
            if rd16(d.add(6)) & PCNET_TX_OWN == 0 {
                self.tx_pos = ((t + 1) % PCNET_RING) as u32;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("nic: pcnet tx timeout")
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

/// Returns the first memory-mapped PCI BAR of `dev` as `(bar_value, index)`.
fn pick_memory_bar(dev: &PciDevice) -> Option<(u32, usize)> {
    for (index, bar) in dev.bars.iter().enumerate() {
        if *bar == 0 {
            continue;
        }
        if *bar & 1 == 0 {
            return Some((*bar, index));
        }
    }
    None
}

/// Byte offset of the virtio available ring for a queue of `qsize` entries.
const fn virtio_avail_off(qsize: usize) -> usize {
    16 * qsize
}

/// Byte offset of the virtio used ring (next 4 KiB boundary after available).
const fn virtio_used_off(qsize: usize) -> usize {
    let end = 16 * qsize + 6 + 2 * qsize;
    (end + 4095) & !4095
}

/// Selects `csr` on the pcnet RAP and reads it through the RDP window.
///
/// # Safety
/// Raw 16-bit I/O port access.
unsafe fn pcnet_read_csr(iobase: u16, csr: u16) -> u16 {
    port::outw(iobase + PCNET_RAP, csr);
    port::inw(iobase + PCNET_RDP)
}

/// Selects `csr` on the pcnet RAP and writes it through the RDP window.
///
/// # Safety
/// Raw 16-bit I/O port access.
unsafe fn pcnet_write_csr(iobase: u16, csr: u16, val: u16) {
    port::outw(iobase + PCNET_RAP, csr);
    port::outw(iobase + PCNET_RDP, val);
}

/// Selects `bcr` on the pcnet RAP and writes it through the BDP window.
///
/// # Safety
/// Raw 16-bit I/O port access.
unsafe fn pcnet_write_bcr(iobase: u16, bcr: u16, val: u16) {
    port::outw(iobase + PCNET_RAP, bcr);
    port::outw(iobase + PCNET_BDP, val);
}

/// 8-bit MMIO read at `base + off`.
///
/// # Safety
/// Raw volatile read; `base` must be a mapped device register range.
#[inline]
unsafe fn mmio_r8(base: u64, off: u64) -> u8 {
    ((base + off) as *const u8).read_volatile()
}

/// 8-bit MMIO write at `base + off`.
///
/// # Safety
/// Raw volatile write; `base` must be a mapped device register range.
#[inline]
unsafe fn mmio_w8(base: u64, off: u64, val: u8) {
    ((base + off) as *mut u8).write_volatile(val)
}

/// 16-bit MMIO write at `base + off`.
///
/// # Safety
/// Raw volatile write; `base` must be a mapped device register range.
#[inline]
unsafe fn mmio_w16(base: u64, off: u64, val: u16) {
    ((base + off) as *mut u16).write_volatile(val)
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

/// 16-bit volatile read of a DMA byte pointer.
///
/// # Safety
/// `p` must point into mapped, aligned DMA memory.
#[inline]
unsafe fn rd16(p: *const u8) -> u16 {
    ptr::read_volatile(p as *const u16)
}

/// 16-bit volatile write of a DMA byte pointer.
///
/// # Safety
/// `p` must point into mapped, aligned DMA memory.
#[inline]
unsafe fn wr16(p: *mut u8, v: u16) {
    ptr::write_volatile(p as *mut u16, v)
}

/// 32-bit volatile read of a DMA byte pointer.
///
/// # Safety
/// `p` must point into mapped, aligned DMA memory.
#[inline]
unsafe fn rd32(p: *const u8) -> u32 {
    ptr::read_volatile(p as *const u32)
}

/// 32-bit volatile write of a DMA byte pointer.
///
/// # Safety
/// `p` must point into mapped, aligned DMA memory.
#[inline]
unsafe fn wr32(p: *mut u8, v: u32) {
    ptr::write_volatile(p as *mut u32, v)
}

/// 64-bit volatile write of a DMA byte pointer.
///
/// # Safety
/// `p` must point into mapped, aligned DMA memory.
#[inline]
unsafe fn wr64(p: *mut u8, v: u64) {
    ptr::write_volatile(p as *mut u64, v)
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
