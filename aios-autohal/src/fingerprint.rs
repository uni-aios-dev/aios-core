use aios_hal::hardware::{HardwareProfile, StorageInterface};
use serde::{Deserialize, Serialize};

/// The physical bus a device is attached to. Every supported bus participates
/// in the hardware inspector tree and the driver lookup key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BusType {
    USB,
    PCI,
    Bluetooth,
    ACPI,
    NVMe,
}

impl BusType {
    /// Lowercase bus tag used in driver ids, e.g. `usb`, `pci`, `nvme`.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::USB => "usb",
            Self::PCI => "pci",
            Self::Bluetooth => "bt",
            Self::ACPI => "acpi",
            Self::NVMe => "nvme",
        }
    }

    /// Human-readable bus name for the UI tree.
    pub fn label(&self) -> &'static str {
        match self {
            Self::USB => "USB",
            Self::PCI => "PCI",
            Self::Bluetooth => "Bluetooth",
            Self::ACPI => "ACPI",
            Self::NVMe => "NVMe",
        }
    }
}

impl std::fmt::Display for BusType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A device "fingerprint": the minimal tuple that identifies a piece of
/// hardware and drives the local lookup, the remote fetch and the persisted
/// `Fingerprint -> DriverID` index entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HardwareFingerprint {
    pub bus: BusType,
    /// e.g. `0x046D` (Logitech).
    pub vendor_id: u16,
    /// e.g. `0x0825` (C270 webcam).
    pub device_id: u16,
    /// Device class code (PCI base class `<< 16 | subclass << 8`), 0 when unknown.
    pub class_code: u32,
    /// Serial number (USB) or ACPI identifier when available.
    pub serial_or_acpi: Option<String>,
}

impl HardwareFingerprint {
    /// Stable lookup key, e.g. `usb.046d.0825` or `acpi.SN12345`.
    pub fn key(&self) -> String {
        let vid = format!("{:04x}", self.vendor_id);
        let did = format!("{:04x}", self.device_id);
        match (self.bus, self.serial_or_acpi.as_deref()) {
            (BusType::ACPI, Some(serial)) => format!("acpi.{serial}"),
            (BusType::USB, Some(serial)) => format!("usb.{vid}.{did}.{serial}"),
            _ => format!("{}.{vid}.{did}", self.bus.tag()),
        }
    }

    /// Canonical driver id for the device, e.g. `driver.usb.046d.0825`.
    pub fn driver_id(&self) -> String {
        match (self.bus, self.serial_or_acpi.as_deref()) {
            (BusType::ACPI, Some(serial)) => format!("driver.acpi.{serial}"),
            _ => format!(
                "driver.{}.{:04x}.{:04x}",
                self.bus.tag(),
                self.vendor_id,
                self.device_id
            ),
        }
    }

    /// Compact human-readable form used in toasts, e.g. `USB 046D:0825`.
    pub fn display_name(&self) -> String {
        match self.serial_or_acpi.as_deref() {
            Some(serial) if self.bus == BusType::ACPI => format!("ACPI {serial}"),
            Some(serial) => format!(
                "{} {:04X}:{:04X} ({serial})",
                self.bus.label(),
                self.vendor_id,
                self.device_id
            ),
            None => format!(
                "{} {:04X}:{:04X}",
                self.bus.label(),
                self.vendor_id,
                self.device_id
            ),
        }
    }

    /// `true` when the fingerprint carries enough identification to drive a
    /// lookup (either a vendor/device pair or an ACPI/serial id).
    pub fn is_actionable(&self) -> bool {
        match self.bus {
            BusType::ACPI => self.serial_or_acpi.is_some(),
            _ => self.vendor_id != 0 || self.device_id != 0,
        }
    }
}

impl std::fmt::Display for HardwareFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// Extract fingerprints from an `aios-hal` hardware snapshot.
///
/// * USB devices -> `BusType::USB` (VID/PID from the HAL scan);
/// * PCI devices  -> `BusType::PCI` (class/subclass folded into `class_code`);
/// * NVMe storage -> `BusType::NVMe` (no VID/PID in the HAL scan, class
///   `0x010802` mass-storage and the model used as an identifier).
///
/// Bluetooth/ACPI devices are not yet surfaced by `aios-hal`; the variants
/// exist so the inspector tree and lookup keys already cover them.
pub fn extract_fingerprints(profile: &HardwareProfile) -> Vec<HardwareFingerprint> {
    let mut out = Vec::new();

    for usb in &profile.usb_devices {
        if usb.is_hub {
            continue;
        }
        out.push(HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: usb.vendor_id,
            device_id: usb.product_id,
            class_code: 0,
            serial_or_acpi: (!usb.port.is_empty()).then(|| usb.port.clone()),
        });
    }

    for pci in &profile.pci_devices {
        out.push(HardwareFingerprint {
            bus: BusType::PCI,
            vendor_id: pci.vendor_id,
            device_id: pci.device_id,
            class_code: (u32::from(pci.class) << 16) | (u32::from(pci.subclass) << 8),
            serial_or_acpi: None,
        });
    }

    for storage in &profile.storage_devices {
        if storage.interface != StorageInterface::NVMe {
            continue;
        }
        out.push(HardwareFingerprint {
            bus: BusType::NVMe,
            vendor_id: 0,
            device_id: 0,
            class_code: 0x010802,
            serial_or_acpi: Some(storage.model.clone()),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_hal::hardware::{PciDevice, StorageDevice, UsbDevice};

    fn profile() -> HardwareProfile {
        HardwareProfile {
            usb_devices: vec![UsbDevice {
                name: "Logitech C270 Webcam".into(),
                vendor_id: 0x046D,
                product_id: 0x0825,
                speed: aios_hal::hardware::UsbSpeed::Usb20,
                is_hub: false,
                port: "1-1.4".into(),
            }],
            pci_devices: vec![PciDevice {
                vendor_id: 0x10DE,
                device_id: 0x2684,
                class: 3,
                subclass: 0,
                name: "NVIDIA RTX 4090".into(),
            }],
            storage_devices: vec![StorageDevice {
                name: "Samsung 990 PRO".into(),
                interface: StorageInterface::NVMe,
                capacity_gb: 2048,
                model: "Samsung 990 PRO 2TB".into(),
            }],
            ..HardwareProfile::mock_modern()
        }
    }

    #[test]
    fn test_extract_usb_pci_nvme() {
        let fps = extract_fingerprints(&profile());
        let buses: Vec<BusType> = fps.iter().map(|f| f.bus).collect();
        assert!(buses.contains(&BusType::USB));
        assert!(buses.contains(&BusType::PCI));
        assert!(buses.contains(&BusType::NVMe));
    }

    #[test]
    fn test_usb_key_and_driver_id() {
        let fp = &extract_fingerprints(&profile())[0];
        assert_eq!(fp.bus, BusType::USB);
        assert_eq!(fp.vendor_id, 0x046D);
        assert_eq!(fp.device_id, 0x0825);
        assert_eq!(fp.driver_id(), "driver.usb.046d.0825");
        assert!(fp.key().starts_with("usb.046d.0825"));
    }

    #[test]
    fn test_pci_class_code() {
        let fp = extract_fingerprints(&profile())
            .into_iter()
            .find(|f| f.bus == BusType::PCI)
            .unwrap();
        assert_eq!(fp.class_code, 0x030000);
    }

    #[test]
    fn test_actionable() {
        let fp = HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x046D,
            device_id: 0x0825,
            class_code: 0,
            serial_or_acpi: None,
        };
        assert!(fp.is_actionable());
        let empty = HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0,
            device_id: 0,
            class_code: 0,
            serial_or_acpi: None,
        };
        assert!(!empty.is_actionable());
    }

    #[test]
    fn test_serialization() {
        let fp = HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x046D,
            device_id: 0x0825,
            class_code: 0,
            serial_or_acpi: Some("1-1".into()),
        };
        let bytes = bincode::serialize(&fp).unwrap();
        let restored: HardwareFingerprint = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored, fp);
    }

    #[test]
    fn test_fingerprint_extract_speed() {
        let profile = profile();
        let per_op_limit_ns: u128 = if cfg!(debug_assertions) {
            50_000
        } else {
            8_000
        };
        for _ in 0..100 {
            let fps = extract_fingerprints(&profile);
            assert_eq!(fps.len(), 3);
        }
        let start = std::time::Instant::now();
        let mut ops = 0u32;
        while start.elapsed().as_micros() < 20_000 {
            let fps = extract_fingerprints(&profile);
            for fp in &fps {
                let _ = fp.key();
                let _ = fp.driver_id();
            }
            ops += 1;
        }
        let per_op_ns = start.elapsed().as_nanos() / u128::from(ops);
        assert!(
            per_op_ns < per_op_limit_ns,
            "extract+key+driver_id took {per_op_ns}ns per op, limit {per_op_limit_ns}ns"
        );
    }
}
