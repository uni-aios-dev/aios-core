//! PCI configuration-space enumeration over the legacy `0xCF8`/`0xCFC` I/O
//! ports.
//!
//! This is the discovery layer the bare-metal storage (AHCI/NVMe) and input
//! (xHCI) drivers will build on: it walks every bus/device/function, records
//! the vendor/device ids, class codes and Base Address Registers into a
//! caller-provided fixed array, and can decode a class into a human name.

use crate::port::{inl, outl};

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// Vendor id returned for a function that does not exist.
pub const NO_DEVICE: u16 = 0xFFFF;
/// Class code: mass storage controller.
pub const CLASS_STORAGE: u8 = 0x01;
/// Class code: serial bus controller (USB, SMBus, ...).
pub const CLASS_SERIAL_BUS: u8 = 0x0C;

/// A single PCI function discovered on the bus.
#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
    pub bars: [u32; 6],
    pub irq: u8,
}

impl PciDevice {
    /// Blank slot used to size fixed arrays without allocating.
    pub const EMPTY: PciDevice = PciDevice {
        bus: 0,
        device: 0,
        function: 0,
        vendor_id: 0,
        device_id: 0,
        class: 0,
        subclass: 0,
        prog_if: 0,
        header_type: 0,
        bars: [0; 6],
        irq: 0,
    };

    /// Whether this device is a mass-storage controller.
    pub fn is_storage(&self) -> bool {
        self.class == CLASS_STORAGE
    }

    /// Whether this device is a serial-bus (USB) controller.
    pub fn is_usb(&self) -> bool {
        self.class == CLASS_SERIAL_BUS
    }

    /// Human-readable class name for the class/subclass pair.
    pub fn class_name(&self) -> &'static str {
        class_name(self.class, self.subclass)
    }
}

/// Reads a 32-bit dword from `(bus, device, function, offset)`.
///
/// # Safety
/// Performs raw port I/O; only valid on x86 with the legacy config mechanism.
pub unsafe fn config_read32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC);
    outl(CONFIG_ADDRESS, address);
    inl(CONFIG_DATA)
}

/// Reads a 16-bit word from `(bus, device, function, offset)`.
///
/// # Safety
/// Performs raw port I/O.
pub unsafe fn config_read16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let dword = config_read32(bus, device, function, offset & 0xFC);
    ((dword >> ((offset as u32 & 2) * 8)) & 0xFFFF) as u16
}

/// Reads an 8-bit byte from `(bus, device, function, offset)`.
///
/// # Safety
/// Performs raw port I/O.
pub unsafe fn config_read8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let dword = config_read32(bus, device, function, offset & 0xFC);
    ((dword >> ((offset as u32 & 3) * 8)) & 0xFF) as u8
}

/// Writes a 32-bit dword to `(bus, device, function, offset)`.
///
/// # Safety
/// Performs raw port I/O; used to program BARs and enable bus mastering.
pub unsafe fn config_write32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC);
    outl(CONFIG_ADDRESS, address);
    outl(CONFIG_DATA, value);
}

unsafe fn probe_function(bus: u8, device: u8, function: u8) -> Option<PciDevice> {
    let vendor_id = config_read16(bus, device, function, 0x00);
    if vendor_id == NO_DEVICE {
        return None;
    }
    let device_id = config_read16(bus, device, function, 0x02);
    let class = config_read8(bus, device, function, 0x0B);
    let subclass = config_read8(bus, device, function, 0x0A);
    let prog_if = config_read8(bus, device, function, 0x09);
    let header_type = config_read8(bus, device, function, 0x0E);
    let irq = config_read8(bus, device, function, 0x3C);

    let mut bars = [0u32; 6];
    if header_type & 0x7F == 0x00 {
        for (i, bar) in bars.iter_mut().enumerate() {
            *bar = config_read32(bus, device, function, 0x10 + (i as u8) * 4);
        }
    }

    Some(PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id,
        class,
        subclass,
        prog_if,
        header_type,
        bars,
        irq,
    })
}

/// Walks the whole PCI configuration space and records each function found.
///
/// Returns the number of devices written to `out` (truncated if the slice is
/// too small).
///
/// # Safety
/// Performs raw port I/O.
pub unsafe fn enumerate(out: &mut [PciDevice]) -> usize {
    let mut count = 0usize;
    for bus in 0u16..=255 {
        for device in 0u8..32 {
            let Some(first) = probe_function(bus as u8, device, 0) else {
                continue;
            };
            let multifunction = first.header_type & 0x80 != 0;
            if count < out.len() {
                out[count] = first;
                count += 1;
            }
            if !multifunction {
                continue;
            }
            for function in 1u8..8 {
                if let Some(dev) = probe_function(bus as u8, device, function) {
                    if count < out.len() {
                        out[count] = dev;
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

/// Decodes a PCI class/subclass pair into a short human-readable name.
pub fn class_name(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x00, _) => "unclassified",
        (0x01, 0x00) => "SCSI",
        (0x01, 0x01) => "IDE",
        (0x01, 0x06) => "SATA/AHCI",
        (0x01, 0x08) => "NVMe",
        (0x01, _) => "storage",
        (0x02, _) => "network",
        (0x03, _) => "display",
        (0x04, _) => "multimedia",
        (0x05, _) => "memory controller",
        (0x06, 0x00) => "host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI bridge",
        (0x06, _) => "bridge",
        (0x07, _) => "serial (UART/parallel)",
        (0x08, _) => "system peripheral",
        (0x09, _) => "input controller",
        (0x0A, _) => "docking station",
        (0x0B, _) => "processor",
        (0x0C, 0x03) => "USB",
        (0x0C, _) => "serial bus",
        (0x0D, _) => "wireless",
        (0x0E, _) => "intelligent I/O",
        (0x0F, _) => "satellite",
        (0x10, _) => "encryption",
        (0x11, _) => "signal processing",
        (0xFF, _) => "unassigned",
        _ => "unknown",
    }
}
