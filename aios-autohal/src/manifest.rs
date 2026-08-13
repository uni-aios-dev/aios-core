use aios_security::capability::Capability;
use serde::{Deserialize, Serialize};

use crate::fingerprint::HardwareFingerprint;

/// The upstream origin of a driver. Shown verbatim in both UIs so the source
/// column is identical across TUI and GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DriverSource {
    RedoxTree,
    LinuxCore,
    CustomStore,
    Builtin,
    GenericFallback,
}

impl DriverSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::RedoxTree => "Redox Tree",
            Self::LinuxCore => "Linux Core",
            Self::CustomStore => "Custom Store",
            Self::Builtin => "Builtin",
            Self::GenericFallback => "Generic",
        }
    }
}

impl std::fmt::Display for DriverSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// One hardware pattern a manifest claims to support.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SupportedHardware {
    /// Bus tag, e.g. `usb`, `pci`, `nvme`, `bt`, `acpi`.
    pub bus: String,
    /// Optional vendor id (wildcard match when `None`).
    pub vendor_id: Option<u16>,
    /// Optional device id (wildcard match when `None`).
    pub device_id: Option<u16>,
}

impl SupportedHardware {
    pub fn matches(&self, fp: &HardwareFingerprint) -> bool {
        if !self.bus.eq_ignore_ascii_case(fp.bus.tag()) {
            return false;
        }
        if let Some(vid) = self.vendor_id {
            if vid != fp.vendor_id {
                return false;
            }
        }
        if let Some(did) = self.device_id {
            if did != fp.device_id {
                return false;
            }
        }
        true
    }
}

/// Resolve a `CAP_*` token string to its typed [`Capability`], if known.
pub fn cap_from_name(name: &str) -> Option<Capability> {
    let name = name.trim();
    match name {
        "CAP_NET_BIND" => Some(Capability::NetBind),
        "CAP_NET_CONNECT" => Some(Capability::NetConnect),
        "CAP_NET_LISTEN" => Some(Capability::NetListen),
        "CAP_FS_READ" => Some(Capability::FsRead),
        "CAP_FS_WRITE" => Some(Capability::FsWrite),
        "CAP_FS_DELETE" => Some(Capability::FsDelete),
        "CAP_HW_ACCESS" => Some(Capability::HwAccess),
        "CAP_MEM_ALLOC" => Some(Capability::MemAlloc),
        "CAP_MEM_SHARE" => Some(Capability::MemShare),
        "CAP_SCHED_MODIFY" => Some(Capability::SchedModify),
        "CAP_BLOCK_LOAD" => Some(Capability::BlockLoad),
        "CAP_BLOCK_UNLOAD" => Some(Capability::BlockUnload),
        "CAP_PROCESS_SPAWN" => Some(Capability::ProcessSpawn),
        "CAP_PROCESS_KILL" => Some(Capability::ProcessKill),
        "CAP_SYSTEM_CONFIG" => Some(Capability::SystemConfig),
        "CAP_ALL" => Some(Capability::All),
        _ => None,
    }
}

mod cap_serde {
    use super::cap_from_name;
    use aios_security::capability::Capability;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(caps: &[Capability], ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let names: Vec<&str> = caps.iter().map(Capability::name).collect();
        serde::Serialize::serialize(&names, ser)
    }

    pub fn deserialize<'de, D>(de: D) -> Result<Vec<Capability>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let names: Vec<String> = serde::Deserialize::deserialize(de)?;
        let mut caps = Vec::with_capacity(names.len());
        for name in names {
            let cap = cap_from_name(&name)
                .ok_or_else(|| serde::de::Error::custom(format!("unknown capability: {name}")))?;
            caps.push(cap);
        }
        Ok(caps)
    }
}

/// `driver.json` — the manifest of a WASM device driver.
///
/// The schema is versioned through `schema_version` so future fields can be
/// added without breaking already-cached drivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverManifest {
    /// Unique string id, e.g. `driver.usb.046d.0825`.
    pub id: String,
    /// Human-readable driver name.
    pub name: String,
    /// Semantic version.
    pub version: String,
    /// Free-form description shown in the UI.
    pub description: String,
    /// Hardware patterns this driver serves.
    pub supported_hardware: Vec<SupportedHardware>,
    /// Access tokens the driver is granted in the sandbox, e.g.
    /// `["CAP_HW_ACCESS", "CAP_NET_BIND", "CAP_MEM_ALLOC"]`.
    #[serde(with = "cap_serde")]
    pub required_capabilities: Vec<Capability>,
    /// SHA-256 of the WASM binary the manifest describes.
    pub hash_sha256: String,
    /// Name of the primary exported function, e.g. `_start_driver`.
    pub entry_point: String,
    /// Upstream source of the driver.
    pub source: DriverSource,
    /// Size in bytes of the WASM binary.
    pub size_bytes: u64,
}

impl DriverManifest {
    /// A manifest serves a fingerprint when at least one supported-hardware
    /// pattern matches it.
    pub fn can_serve(&self, fp: &HardwareFingerprint) -> bool {
        self.supported_hardware.iter().any(|s| s.matches(fp))
    }

    /// The `CAP_*` token names, in manifest order (used by the UI matrix).
    pub fn capability_names(&self) -> Vec<String> {
        self.required_capabilities
            .iter()
            .map(|c| c.name().to_string())
            .collect()
    }

    /// Structural validation: id/version/entry point non-empty and the hash is
    /// a 64-hex-char SHA-256.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("manifest id must not be empty".into());
        }
        if self.name.is_empty() {
            return Err("manifest name must not be empty".into());
        }
        if self.version.is_empty() {
            return Err("manifest version must not be empty".into());
        }
        if self.entry_point.is_empty() {
            return Err("manifest entry point must not be empty".into());
        }
        if !self.hash_sha256.is_empty()
            && (self.hash_sha256.len() != 64
                || !self.hash_sha256.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("hash_sha256 must be a 64-character hex SHA-256".into());
        }
        Ok(())
    }

    /// Serialize to compact JSON.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| e.to_string())
    }

    /// Serialize to pretty-printed JSON (used for `driver.json` on disk).
    pub fn to_json_pretty(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }

    /// Parse a manifest from JSON bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let m: DriverManifest = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        m.validate()?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BusType;

    fn manifest() -> DriverManifest {
        DriverManifest {
            id: "driver.usb.046d.0825".into(),
            name: "Logitech C270 Webcam Driver".into(),
            version: "1.2.0".into(),
            description: "UVC webcam driver".into(),
            supported_hardware: vec![SupportedHardware {
                bus: "usb".into(),
                vendor_id: Some(0x046D),
                device_id: Some(0x0825),
            }],
            required_capabilities: vec![Capability::HwAccess, Capability::MemAlloc],
            hash_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
            entry_point: "_start_driver".into(),
            source: DriverSource::CustomStore,
            size_bytes: 4096,
        }
    }

    #[test]
    fn test_manifest_roundtrip_json() {
        let m = manifest();
        let json = m.to_json_pretty().unwrap();
        assert!(json.contains("\"required_capabilities\": ["));
        assert!(json.contains("CAP_HW_ACCESS"));
        let back = DriverManifest::from_json(json.as_bytes()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn test_manifest_can_serve_matching() {
        let m = manifest();
        let fp = HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x046D,
            device_id: 0x0825,
            class_code: 0,
            serial_or_acpi: None,
        };
        assert!(m.can_serve(&fp));
    }

    #[test]
    fn test_manifest_rejects_foreign_device() {
        let m = manifest();
        let fp = HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x8087,
            device_id: 0x0024,
            class_code: 0,
            serial_or_acpi: None,
        };
        assert!(!m.can_serve(&fp));
    }

    #[test]
    fn test_manifest_wildcard_bus_match() {
        let m = DriverManifest {
            supported_hardware: vec![SupportedHardware {
                bus: "usb".into(),
                vendor_id: None,
                device_id: None,
            }],
            ..manifest()
        };
        let fp = HardwareFingerprint {
            bus: BusType::USB,
            vendor_id: 0x1234,
            device_id: 0x5678,
            class_code: 0,
            serial_or_acpi: None,
        };
        assert!(m.can_serve(&fp));
    }

    #[test]
    fn test_validate_rejects_bad_hash() {
        let mut m = manifest();
        m.hash_sha256 = "not-a-hash".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_unknown_capability_rejected() {
        let json = r#"{
            "id": "driver.usb.1111.2222",
            "name": "x",
            "version": "1.0.0",
            "description": "",
            "supported_hardware": [],
            "required_capabilities": ["CAP_DOES_NOT_EXIST"],
            "hash_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "entry_point": "_start_driver",
            "source": "custom-store",
            "size_bytes": 1
        }"#;
        assert!(DriverManifest::from_json(json.as_bytes()).is_err());
    }

    #[test]
    fn test_capability_name_roundtrip() {
        for cap in Capability::all_variants() {
            assert_eq!(cap_from_name(cap.name()), Some(cap));
        }
        assert_eq!(cap_from_name("CAP_ALL"), Some(Capability::All));
        assert_eq!(cap_from_name("CAP_NOPE"), None);
    }

    #[test]
    fn test_source_labels() {
        assert_eq!(DriverSource::RedoxTree.label(), "Redox Tree");
        assert_eq!(DriverSource::LinuxCore.label(), "Linux Core");
        assert_eq!(DriverSource::CustomStore.label(), "Custom Store");
        assert_eq!(DriverSource::GenericFallback.label(), "Generic");
    }
}
