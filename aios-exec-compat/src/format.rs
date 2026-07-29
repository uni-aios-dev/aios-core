use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExecutableType {
    AiosNative,
    LinuxElf,
    WindowsPe,
    Unknown,
}

impl ExecutableType {
    pub fn from_bytes(data: &[u8]) -> Self {
        if data.len() < 4 {
            return Self::Unknown;
        }
        match data {
            [0x7f, b'E', b'L', b'F', ..] => Self::LinuxElf,
            [b'M', b'Z', ..] => Self::WindowsPe,
            [b'A', b'I', b'O', b'S', ..] => Self::AiosNative,
            _ => Self::Unknown,
        }
    }

    pub fn from_extension(path: &str) -> Self {
        let lower = path.to_lowercase();
        if lower.ends_with(".exe") || lower.ends_with(".dll") || lower.ends_with(".sys") {
            Self::WindowsPe
        } else if lower.ends_with(".so") || lower.ends_with(".elf") || lower.ends_with(".bin") {
            Self::LinuxElf
        } else if lower.ends_with(".aib") {
            Self::AiosNative
        } else {
            Self::Unknown
        }
    }

    pub fn subsystem_name(&self) -> &'static str {
        match self {
            Self::AiosNative => "aios-native",
            Self::LinuxElf => "aios-subsystem-posix",
            Self::WindowsPe => "aios-subsystem-win32",
            Self::Unknown => "unknown",
        }
    }

    pub fn required_capabilities(&self) -> Vec<CompatCapability> {
        match self {
            Self::AiosNative => vec![],
            Self::LinuxElf => vec![
                CompatCapability::FilesystemRead,
                CompatCapability::FilesystemWrite,
                CompatCapability::ProcessCreate,
                CompatCapability::NetworkAccess,
            ],
            Self::WindowsPe => vec![
                CompatCapability::FilesystemRead,
                CompatCapability::FilesystemWrite,
                CompatCapability::ProcessCreate,
                CompatCapability::NetworkAccess,
                CompatCapability::RegistryAccess,
                CompatCapability::WinApiCompat,
            ],
            Self::Unknown => vec![],
        }
    }
}

impl fmt::Display for ExecutableType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AiosNative => write!(f, "AIOS Native"),
            Self::LinuxElf => write!(f, "Linux ELF"),
            Self::WindowsPe => write!(f, "Windows PE"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompatCapability {
    FilesystemRead,
    FilesystemWrite,
    ProcessCreate,
    NetworkAccess,
    RegistryAccess,
    WinApiCompat,
    PosixCompat,
    MemoryMap,
    ThreadCreate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryHeader {
    pub executable_type: ExecutableType,
    pub entry_point_offset: u64,
    pub image_base: u64,
    pub is_64bit: bool,
    pub subsystem: u16,
    pub machine_arch: u16,
}

impl BinaryHeader {
    pub fn parse(data: &[u8]) -> Self {
        let executable_type = ExecutableType::from_bytes(data);
        match executable_type {
            ExecutableType::LinuxElf => Self::parse_elf(data),
            ExecutableType::WindowsPe => Self::parse_pe(data),
            ExecutableType::AiosNative => Self::parse_aios(data),
            _ => Self::unknown(),
        }
    }

    fn parse_elf(data: &[u8]) -> Self {
        let is_64bit = data.len() > 4 && data[4] == 2;
        let entry_point_offset = if is_64bit && data.len() >= 24 {
            u64::from_le_bytes(data[24..32].try_into().unwrap_or([0; 8]))
        } else if !is_64bit && data.len() >= 24 {
            u32::from_le_bytes(data[24..28].try_into().unwrap_or([0; 4])) as u64
        } else {
            0
        };
        let machine_arch = if is_64bit { 0x3E } else { 0x03 };
        Self {
            executable_type: ExecutableType::LinuxElf,
            entry_point_offset,
            image_base: 0,
            is_64bit,
            subsystem: 0,
            machine_arch,
        }
    }

    fn parse_pe(data: &[u8]) -> Self {
        let pe_offset = if data.len() >= 64 {
            u32::from_le_bytes(data[60..64].try_into().unwrap_or([0; 4])) as usize
        } else {
            0
        };
        let is_64bit = if pe_offset + 24 < data.len() {
            data[pe_offset + 24] == 0x20
        } else {
            false
        };
        let entry_point_offset = if pe_offset + 40 < data.len() {
            let ep = u32::from_le_bytes(
                data[pe_offset + 40..pe_offset + 44]
                    .try_into()
                    .unwrap_or([0; 4]),
            );
            ep as u64
        } else {
            0
        };
        let machine_arch = if pe_offset + 2 < data.len() {
            u16::from_le_bytes(data[pe_offset..pe_offset + 2].try_into().unwrap_or([0; 2]))
        } else {
            0
        };
        Self {
            executable_type: ExecutableType::WindowsPe,
            entry_point_offset,
            image_base: 0,
            is_64bit,
            subsystem: 3,
            machine_arch,
        }
    }

    fn parse_aios(data: &[u8]) -> Self {
        let is_64bit = data.len() > 5 && data[4] == 1;
        Self {
            executable_type: ExecutableType::AiosNative,
            entry_point_offset: if data.len() >= 16 {
                u64::from_le_bytes(data[8..16].try_into().unwrap_or([0; 8]))
            } else {
                0
            },
            image_base: 0,
            is_64bit,
            subsystem: 0xFF,
            machine_arch: 0xA05,
        }
    }

    fn unknown() -> Self {
        Self {
            executable_type: ExecutableType::Unknown,
            entry_point_offset: 0,
            image_base: 0,
            is_64bit: false,
            subsystem: 0,
            machine_arch: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elf_magic() {
        let mut data = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&1024u64.to_le_bytes());
        let hdr = BinaryHeader::parse(&data);
        assert_eq!(hdr.executable_type, ExecutableType::LinuxElf);
        assert!(hdr.is_64bit);
        assert_eq!(hdr.entry_point_offset, 1024);
    }

    #[test]
    fn test_elf32() {
        let mut data = vec![0x7f, b'E', b'L', b'F', 1, 1, 1, 0];
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&2048u32.to_le_bytes());
        let hdr = BinaryHeader::parse(&data);
        assert_eq!(hdr.executable_type, ExecutableType::LinuxElf);
        assert!(!hdr.is_64bit);
        assert_eq!(hdr.entry_point_offset, 2048);
    }

    #[test]
    fn test_pe_magic() {
        let mut data = vec![b'M', b'Z'];
        data.extend_from_slice(&[0u8; 58]);
        data.extend_from_slice(&64u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 16]);
        data.resize(64 + 44, 0);
        data[64..66].copy_from_slice(&0x8664u16.to_le_bytes());
        data[64 + 24] = 0x20;
        data[64 + 40..64 + 44].copy_from_slice(&4096u32.to_le_bytes());
        let hdr = BinaryHeader::parse(&data);
        assert_eq!(hdr.executable_type, ExecutableType::WindowsPe);
        assert!(hdr.is_64bit);
        assert_eq!(hdr.entry_point_offset, 4096);
        assert_eq!(hdr.machine_arch, 0x8664);
    }

    #[test]
    fn test_pe32() {
        let mut data = vec![b'M', b'Z'];
        data.extend_from_slice(&[0u8; 58]);
        data.extend_from_slice(&64u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 16]);
        data.resize(64 + 44, 0);
        data[64..66].copy_from_slice(&0x014Cu16.to_le_bytes());
        data[64 + 24] = 0x0B;
        data[64 + 40..64 + 44].copy_from_slice(&8192u32.to_le_bytes());
        let hdr = BinaryHeader::parse(&data);
        assert_eq!(hdr.executable_type, ExecutableType::WindowsPe);
        assert!(!hdr.is_64bit);
        assert_eq!(hdr.entry_point_offset, 8192);
    }

    #[test]
    fn test_aios_native() {
        let mut data = vec![b'A', b'I', b'O', b'S', 1];
        data.extend_from_slice(&[0u8; 3]);
        data.extend_from_slice(&0x1000u64.to_le_bytes());
        data.extend_from_slice(&[0u8; 64]);
        let hdr = BinaryHeader::parse(&data);
        assert_eq!(hdr.executable_type, ExecutableType::AiosNative);
        assert!(hdr.is_64bit);
        assert_eq!(hdr.entry_point_offset, 0x1000);
    }

    #[test]
    fn test_unknown_magic() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let hdr = BinaryHeader::parse(&data);
        assert_eq!(hdr.executable_type, ExecutableType::Unknown);
    }

    #[test]
    fn test_empty_data() {
        let data: [u8; 0] = [];
        let hdr = BinaryHeader::parse(&data);
        assert_eq!(hdr.executable_type, ExecutableType::Unknown);
    }

    #[test]
    fn test_from_extension() {
        assert_eq!(
            ExecutableType::from_extension("test.exe"),
            ExecutableType::WindowsPe
        );
        assert_eq!(
            ExecutableType::from_extension("lib.so"),
            ExecutableType::LinuxElf
        );
        assert_eq!(
            ExecutableType::from_extension("block.aib"),
            ExecutableType::AiosNative
        );
        assert_eq!(
            ExecutableType::from_extension("test.bin"),
            ExecutableType::LinuxElf
        );
        assert_eq!(
            ExecutableType::from_extension("data.txt"),
            ExecutableType::Unknown
        );
    }

    #[test]
    fn test_subsystem_name() {
        assert_eq!(ExecutableType::AiosNative.subsystem_name(), "aios-native");
        assert_eq!(
            ExecutableType::LinuxElf.subsystem_name(),
            "aios-subsystem-posix"
        );
        assert_eq!(
            ExecutableType::WindowsPe.subsystem_name(),
            "aios-subsystem-win32"
        );
        assert_eq!(ExecutableType::Unknown.subsystem_name(), "unknown");
    }

    #[test]
    fn test_required_capabilities_pe() {
        let caps = ExecutableType::WindowsPe.required_capabilities();
        assert!(caps.contains(&CompatCapability::RegistryAccess));
        assert!(caps.contains(&CompatCapability::WinApiCompat));
    }

    #[test]
    fn test_required_capabilities_elf() {
        let caps = ExecutableType::LinuxElf.required_capabilities();
        assert!(
            caps.contains(&CompatCapability::PosixCompat) == false
                || caps.contains(&CompatCapability::ProcessCreate)
        );
        assert!(caps.contains(&CompatCapability::ProcessCreate));
        assert!(caps.contains(&CompatCapability::NetworkAccess));
    }

    #[test]
    fn test_required_capabilities_native() {
        assert!(ExecutableType::AiosNative
            .required_capabilities()
            .is_empty());
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", ExecutableType::AiosNative), "AIOS Native");
        assert_eq!(format!("{}", ExecutableType::LinuxElf), "Linux ELF");
        assert_eq!(format!("{}", ExecutableType::WindowsPe), "Windows PE");
        assert_eq!(format!("{}", ExecutableType::Unknown), "Unknown");
    }

    #[test]
    fn test_header_serialization_roundtrip() {
        let hdr = BinaryHeader {
            executable_type: ExecutableType::LinuxElf,
            entry_point_offset: 4096,
            image_base: 0,
            is_64bit: true,
            subsystem: 0,
            machine_arch: 0x3E,
        };
        let bytes = bincode::serialize(&hdr).unwrap();
        let restored: BinaryHeader = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.executable_type, ExecutableType::LinuxElf);
        assert_eq!(restored.entry_point_offset, 4096);
        assert!(restored.is_64bit);
    }

    #[test]
    fn test_speed_from_bytes() {
        let mut elf = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
        elf.extend_from_slice(&[0u8; 120]);
        let mut pe = vec![b'M', b'Z'];
        pe.extend_from_slice(&[0u8; 58]);
        pe.extend_from_slice(&64u32.to_le_bytes());
        pe.extend_from_slice(&[0u8; 16]);
        pe.resize(300, 0);
        pe[256..258].copy_from_slice(&0x8664u16.to_le_bytes());

        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let _ = ExecutableType::from_bytes(&elf);
            let _ = ExecutableType::from_bytes(&pe);
        }
        let elapsed = start.elapsed();
        let per_us = elapsed.as_micros() as f64 / 20_000.0;
        let threshold = if cfg!(debug_assertions) { 5.0 } else { 1.0 };
        assert!(
            per_us < threshold,
            "Header identification too slow: {per_us} us (threshold: {threshold})"
        );
    }
}
