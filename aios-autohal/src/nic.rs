use crate::fingerprint::{BusType, HardwareFingerprint};

const CLASS_NETWORK: u32 = 0x02;
const SUBCLASS_ETHERNET: u32 = 0x00;
const SUBCLASS_TOKEN_RING: u32 = 0x01;
const SUBCLASS_WIFI: u32 = 0x80;

/// Functional class of a network interface claimed by an AutoHAL driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NicClass {
    /// Wired PCI Ethernet or Token Ring controller.
    Ethernet,
    /// PCI 802.11 controller (class `0x02`, subclass `0x80`).
    Wifi,
}

impl NicClass {
    /// Stable lowercase label used by `driver nics` and the hardware inspector.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ethernet => "ethernet",
            Self::Wifi => "wifi",
        }
    }
}

/// Classifies a PCI class/subclass word folded as `class << 16 | subclass << 8`.
pub fn class_from_code(class_code: u32) -> Option<NicClass> {
    if (class_code >> 16) & 0xFF != CLASS_NETWORK {
        return None;
    }
    match (class_code >> 8) & 0xFF {
        SUBCLASS_ETHERNET | SUBCLASS_TOKEN_RING => Some(NicClass::Ethernet),
        SUBCLASS_WIFI => Some(NicClass::Wifi),
        _ => None,
    }
}

/// Classifies a fingerprint as a NIC, or returns `None` for unrelated hardware.
pub fn classify(fp: &HardwareFingerprint) -> Option<NicClass> {
    if fp.bus != BusType::PCI {
        return None;
    }
    class_from_code(fp.class_code)
}

/// Whether the fingerprint describes a wired or wireless network controller.
pub fn is_nic(fp: &HardwareFingerprint) -> bool {
    classify(fp).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pci(class: u8, subclass: u8) -> HardwareFingerprint {
        HardwareFingerprint {
            bus: BusType::PCI,
            vendor_id: 0x8086,
            device_id: 0x1503,
            class_code: (u32::from(class) << 16) | (u32::from(subclass) << 8),
            serial_or_acpi: None,
        }
    }

    #[test]
    fn class_code_distinguishes_ethernet_and_wifi() {
        assert_eq!(classify(&pci(0x02, 0x00)), Some(NicClass::Ethernet));
        assert_eq!(classify(&pci(0x02, 0x01)), Some(NicClass::Ethernet));
        assert_eq!(classify(&pci(0x02, 0x80)), Some(NicClass::Wifi));
        assert_eq!(classify(&pci(0x03, 0x00)), None);
        assert_eq!(classify(&pci(0x02, 0x05)), None);
    }

    #[test]
    fn non_pci_buses_are_not_classified_as_nics() {
        let mut fp = pci(0x02, 0x00);
        fp.bus = BusType::USB;
        assert!(!is_nic(&fp));
        assert_eq!(NicClass::Ethernet.label(), "ethernet");
        assert_eq!(NicClass::Wifi.label(), "wifi");
    }
}
