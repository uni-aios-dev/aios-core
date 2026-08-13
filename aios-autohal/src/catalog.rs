use aios_security::capability::Capability;
use sha2::{Digest, Sha256};

use crate::fingerprint::HardwareFingerprint;
use crate::manifest::{DriverManifest, DriverSource, SupportedHardware};

/// The embedded generic driver module. It exposes the standard driver surface
/// (`_start_driver`, `init`, `start`) and logs a status line through the
/// `aios.log` host function. It is compiled at runtime by
/// `aios-wasm::WasmBlock::from_wat` and used as the automatic fallback when a
/// dedicated driver crashes or is unavailable — basic safe mode with no
/// capabilities.
pub const GENERIC_WAT: &str = r#"
(module
  (import "aios" "log" (func $log (param i32 i32) (result i32)))
  (import "aios" "get_timestamp" (func $get_timestamp (result i64)))
  (memory (export "memory") 1 4)
  (data (i32.const 16) "driver started")
  (func (export "_start_driver") (result i32)
    i32.const 16
    i32.const 14
    call $log
    drop
    i32.const 1)
  (func (export "init"))
  (func (export "start"))
)
"#;

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A driver that ships inside the AIOS binary and never requires a network
/// fetch: the manifest plus the embedded WAT template it is compiled from.
#[derive(Debug, Clone)]
pub struct BuiltinDriver {
    pub manifest: DriverManifest,
    pub wat: &'static str,
}

impl BuiltinDriver {
    /// WASM bytes used for hash validation are the WAT template itself; the
    /// engine compiles them with `WasmBlock::from_wat`.
    pub fn wat_bytes(&self) -> Vec<u8> {
        self.wat.as_bytes().to_vec()
    }
}

/// The generic fallback driver: always present, grants no capabilities and
/// keeps the device on the bus in a basic safe mode.
pub fn generic_fallback() -> BuiltinDriver {
    BuiltinDriver {
        manifest: DriverManifest {
            id: "driver.generic.fallback".into(),
            name: "Generic Fallback Driver".into(),
            version: "1.0.0".into(),
            description:
                "Built-in safe-mode driver used when a dedicated driver crashes or is unavailable."
                    .into(),
            supported_hardware: vec![SupportedHardware {
                bus: "usb".into(),
                vendor_id: None,
                device_id: None,
            }],
            required_capabilities: Vec::new(),
            hash_sha256: sha256_hex(GENERIC_WAT.as_bytes()),
            entry_point: "_start_driver".into(),
            source: DriverSource::GenericFallback,
            size_bytes: GENERIC_WAT.len() as u64,
        },
        wat: GENERIC_WAT,
    }
}

fn builtin(
    id: &str,
    name: &str,
    bus: &str,
    vendor_id: u16,
    device_id: u16,
    capabilities: Vec<Capability>,
) -> BuiltinDriver {
    BuiltinDriver {
        manifest: DriverManifest {
            id: id.to_string(),
            name: name.to_string(),
            version: "1.0.0".into(),
            description: format!(
                "Offline builtin driver for {name} (VID {:04X}:{:04X}).",
                vendor_id, device_id
            ),
            supported_hardware: vec![SupportedHardware {
                bus: bus.to_string(),
                vendor_id: Some(vendor_id),
                device_id: Some(device_id),
            }],
            required_capabilities: capabilities,
            hash_sha256: sha256_hex(GENERIC_WAT.as_bytes()),
            entry_point: "_start_driver".into(),
            source: DriverSource::Builtin,
            size_bytes: GENERIC_WAT.len() as u64,
        },
        wat: GENERIC_WAT,
    }
}

/// Offline catalog of well-known devices. Serves the two mock profiles used
/// across the workspace tests plus common webcam/GPU/network hardware.
pub fn builtin_catalog() -> Vec<BuiltinDriver> {
    vec![
        builtin(
            "driver.usb.046d.0825",
            "Logitech C270 Webcam",
            "usb",
            0x046D,
            0x0825,
            vec![Capability::HwAccess, Capability::MemAlloc],
        ),
        builtin(
            "driver.usb.046d.0a29",
            "Logitech HD Webcam C270",
            "usb",
            0x046D,
            0x0A29,
            vec![Capability::HwAccess, Capability::MemAlloc],
        ),
        builtin(
            "driver.usb.046d.c52b",
            "Logitech USB Receiver",
            "usb",
            0x046D,
            0xC52B,
            vec![Capability::HwAccess],
        ),
        builtin(
            "driver.pci.10de.2684",
            "NVIDIA GeForce RTX 4090",
            "pci",
            0x10DE,
            0x2684,
            vec![
                Capability::HwAccess,
                Capability::MemAlloc,
                Capability::MemShare,
            ],
        ),
        builtin(
            "driver.pci.8086.1503",
            "Intel Ethernet Controller I210",
            "pci",
            0x8086,
            0x1503,
            vec![
                Capability::HwAccess,
                Capability::NetBind,
                Capability::NetConnect,
            ],
        ),
        builtin(
            "driver.pci.8086.7d0b",
            "Intel AI Boost NPU",
            "pci",
            0x8086,
            0x7D0B,
            vec![Capability::HwAccess, Capability::MemAlloc],
        ),
        builtin(
            "driver.nvme.samsung.990pro",
            "Samsung 990 PRO NVMe",
            "nvme",
            0,
            0,
            vec![
                Capability::HwAccess,
                Capability::FsRead,
                Capability::FsWrite,
            ],
        ),
    ]
}

/// Locate an offline builtin driver for a fingerprint, falling back to the
/// generic driver only when explicitly requested (engine calls this first and
/// the generic fallback separately).
pub fn find_builtin(fp: &HardwareFingerprint) -> Option<BuiltinDriver> {
    builtin_catalog()
        .into_iter()
        .find(|d| d.manifest.can_serve(fp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::BusType;

    #[test]
    fn test_find_logitech_c270() {
        let fp = HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x046D,
            device_id: 0x0825,
            class_code: 0,
            serial_or_acpi: None,
        };
        let driver = find_builtin(&fp).unwrap();
        assert_eq!(driver.manifest.id, "driver.usb.046d.0825");
        assert!(driver.manifest.can_serve(&fp));
    }

    #[test]
    fn test_find_rtx4090() {
        let fp = HardwareFingerprint {
            bus: BusType::PCI,
            vendor_id: 0x10DE,
            device_id: 0x2684,
            class_code: 0x030000,
            serial_or_acpi: None,
        };
        let driver = find_builtin(&fp).unwrap();
        assert_eq!(driver.manifest.source, DriverSource::Builtin);
        assert_eq!(driver.manifest.id, "driver.pci.10de.2684");
    }

    #[test]
    fn test_unknown_device_not_found() {
        let fp = HardwareFingerprint {
            bus: BusType::PCI,
            vendor_id: 0x9999,
            device_id: 0x0001,
            class_code: 0,
            serial_or_acpi: None,
        };
        assert!(find_builtin(&fp).is_none());
    }

    #[test]
    fn test_generic_fallback_has_no_capabilities() {
        let fb = generic_fallback();
        assert_eq!(fb.manifest.id, "driver.generic.fallback");
        assert!(fb.manifest.required_capabilities.is_empty());
        assert_eq!(fb.manifest.source, DriverSource::GenericFallback);
        assert_eq!(fb.manifest.entry_point, "_start_driver");
    }

    #[test]
    fn test_manifest_hash_matches_wat() {
        let fb = generic_fallback();
        let hash = sha256_hex(&fb.wat_bytes());
        assert_eq!(fb.manifest.hash_sha256, hash);
    }

    #[test]
    fn test_catalog_has_nvme_entry() {
        let fp = HardwareFingerprint {
            bus: BusType::NVMe,
            vendor_id: 0,
            device_id: 0,
            class_code: 0x010802,
            serial_or_acpi: Some("Samsung 990 PRO 2TB".into()),
        };
        let driver = find_builtin(&fp).unwrap();
        assert_eq!(driver.manifest.id, "driver.nvme.samsung.990pro");
    }
}
