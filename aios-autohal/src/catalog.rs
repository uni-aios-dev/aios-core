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

/// Embedded WAT for wired and wireless NIC drivers. It exposes the driver
/// surface plus a `net_poll` hook and only uses the two `aios` host functions
/// provided by `aios-wasm`.
pub const NIC_WAT: &str = r#"
(module
  (import "aios" "log" (func $log (param i32 i32) (result i32)))
  (import "aios" "get_timestamp" (func $get_timestamp (result i64)))
  (memory (export "memory") 1 4)
  (data (i32.const 16) "nic driver ready")
  (data (i32.const 48) "nic link poll")
  (func (export "_start_driver") (result i32)
    i32.const 16
    i32.const 16
    call $log
    drop
    i32.const 1)
  (func (export "init"))
  (func (export "start"))
  (func (export "net_poll")
    i32.const 48
    i32.const 13
    call $log
    drop)
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
                class_code: None,
                class_mask: None,
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
                class_code: None,
                class_mask: None,
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

fn builtin_nic(id: &str, name: &str, vendor_id: u16, device_id: u16) -> BuiltinDriver {
    BuiltinDriver {
        manifest: DriverManifest {
            id: id.to_string(),
            name: name.to_string(),
            version: "1.0.0".into(),
            description: format!(
                "Offline NIC driver for {name} (VID {:04X}:{:04X}).",
                vendor_id, device_id
            ),
            supported_hardware: vec![SupportedHardware::new("pci", vendor_id, device_id)],
            required_capabilities: vec![
                Capability::HwAccess,
                Capability::NetBind,
                Capability::NetConnect,
            ],
            hash_sha256: sha256_hex(NIC_WAT.as_bytes()),
            entry_point: "_start_driver".into(),
            source: DriverSource::Builtin,
            size_bytes: NIC_WAT.len() as u64,
        },
        wat: NIC_WAT,
    }
}

fn builtin_nic_class(id: &str, name: &str, class_code: u32, class_mask: u32) -> BuiltinDriver {
    BuiltinDriver {
        manifest: DriverManifest {
            id: id.to_string(),
            name: name.to_string(),
            version: "1.0.0".into(),
            description: format!("Offline NIC driver for PCI class {class_code:06X}."),
            supported_hardware: vec![SupportedHardware::for_class(class_code, class_mask)],
            required_capabilities: vec![
                Capability::HwAccess,
                Capability::NetBind,
                Capability::NetConnect,
            ],
            hash_sha256: sha256_hex(NIC_WAT.as_bytes()),
            entry_point: "_start_driver".into(),
            source: DriverSource::Builtin,
            size_bytes: NIC_WAT.len() as u64,
        },
        wat: NIC_WAT,
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
        builtin_nic(
            "driver.pci.8086.1521",
            "Intel Ethernet Controller I350",
            0x8086,
            0x1521,
        ),
        builtin_nic(
            "driver.pci.10ec.8168",
            "Realtek RTL8168/8111 Gigabit Ethernet",
            0x10EC,
            0x8168,
        ),
        builtin_nic_class(
            "driver.pci.class.ethernet",
            "PCI Ethernet Controller",
            0x020000,
            0xFFFFFF00,
        ),
        builtin_nic_class(
            "driver.pci.class.wifi",
            "PCI 802.11 Controller",
            0x028000,
            0xFFFFFF00,
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
    fn test_catalog_has_nic_class_entries() {
        let ethernet = HardwareFingerprint {
            bus: BusType::PCI,
            vendor_id: 0x1234,
            device_id: 0x5678,
            class_code: 0x020000,
            serial_or_acpi: None,
        };
        let wifi = HardwareFingerprint {
            class_code: 0x028000,
            ..ethernet.clone()
        };
        assert_eq!(
            find_builtin(&ethernet).unwrap().manifest.id,
            "driver.pci.class.ethernet"
        );
        assert_eq!(
            find_builtin(&wifi).unwrap().manifest.id,
            "driver.pci.class.wifi"
        );
    }

    #[test]
    fn test_nic_wat_compiles_and_exposes_net_poll() {
        let driver = builtin_nic(
            "driver.pci.8086.1521",
            "Intel Ethernet Controller I350",
            0x8086,
            0x1521,
        );
        assert_eq!(driver.manifest.hash_sha256, sha256_hex(&driver.wat_bytes()));
        let mut block = aios_wasm::WasmBlock::from_wat(
            "nic-test".into(),
            "1.0.0".into(),
            driver.wat,
            aios_wasm::SandboxConfig::default(),
            aios_wasm::IsolationConfig::restrictive(),
        )
        .unwrap();
        let mut store = block.create_store().unwrap();
        block.instantiate(&mut store).unwrap();
        block.call_func(&mut store, "net_poll", &[]).unwrap();
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
