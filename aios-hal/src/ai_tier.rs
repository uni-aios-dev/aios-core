use crate::hardware::HardwareProfile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiTier {
    Tier1,
    Tier2,
    Tier3,
}

impl AiTier {
    pub fn from_profile(profile: &HardwareProfile) -> Self {
        if profile.npu.is_some()
            && profile.gpu.is_some()
            && profile.cpu.has_avx512
            && profile.memory.total_mb >= 16384
        {
            Self::Tier1
        } else if (profile.cpu.has_avx2 || profile.cpu.has_neon) && profile.memory.total_mb >= 4096
        {
            Self::Tier2
        } else {
            Self::Tier3
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Tier1 => "On-device high-performance local LLM (NPU/GPU + AVX-512)",
            Self::Tier2 => "Quantized SLM via SIMD CPU instructions (AVX2/NEON)",
            Self::Tier3 => "Lightweight deterministic heuristic planner / API fallback",
        }
    }

    pub fn max_model_size_gb(&self) -> f64 {
        match self {
            Self::Tier1 => 70.0,
            Self::Tier2 => 7.0,
            Self::Tier3 => 0.5,
        }
    }

    pub fn recommended_batch_size(&self) -> u32 {
        match self {
            Self::Tier1 => 64,
            Self::Tier2 => 8,
            Self::Tier3 => 1,
        }
    }
}

impl std::fmt::Display for AiTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tier1 => write!(f, "Tier 1 (Local LLM)"),
            Self::Tier2 => write!(f, "Tier 2 (Quantized SLM)"),
            Self::Tier3 => write!(f, "Tier 3 (Heuristic Fallback)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{CpuInfo, CpuVendor, GpuInfo, MemoryInfo, NpuInfo};

    #[test]
    fn test_tier1_modern_hardware() {
        let profile = HardwareProfile::mock_modern();
        assert_eq!(AiTier::from_profile(&profile), AiTier::Tier1);
    }

    #[test]
    fn test_tier2_legacy_with_avx2() {
        let profile = HardwareProfile::mock_legacy();
        assert_eq!(AiTier::from_profile(&profile), AiTier::Tier2);
    }

    #[test]
    fn test_tier3_legacy_2012() {
        let profile = HardwareProfile::mock_legacy_2012();
        assert_eq!(AiTier::from_profile(&profile), AiTier::Tier3);
    }

    #[test]
    fn test_tier3_low_ram() {
        let profile = HardwareProfile {
            cpu: CpuInfo {
                cores: 1,
                threads: 2,
                model: "Old CPU".into(),
                has_avx512: false,
                has_avx2: false,
                has_sse42: false,
                has_neon: false,
                base_freq_mhz: 1200,
                vendor: CpuVendor::Unknown,
            },
            gpu: None,
            npu: None,
            memory: MemoryInfo {
                total_mb: 1024,
                available_mb: 512,
                speed_mhz: 800,
                dimm_count: 1,
            },
            pci_devices: Vec::new(),
            storage_devices: Vec::new(),
            usb_devices: Vec::new(),
            thunderbolt_devices: Vec::new(),
        };
        assert_eq!(AiTier::from_profile(&profile), AiTier::Tier3);
    }

    #[test]
    fn test_tier1_requires_npu() {
        let profile = HardwareProfile {
            cpu: CpuInfo {
                cores: 16,
                threads: 32,
                model: "Fast CPU".into(),
                has_avx512: true,
                has_avx2: true,
                has_sse42: true,
                has_neon: false,
                base_freq_mhz: 4000,
                vendor: CpuVendor::AMD,
            },
            gpu: Some(GpuInfo {
                name: "RTX 4090".into(),
                vram_mb: 24576,
                compute_shaders: true,
                vendor: "NVIDIA".into(),
                driver_version: "546.33".into(),
                cuda_cores: 16384,
                compute_capability: "8.9".into(),
            }),
            npu: None,
            memory: MemoryInfo {
                total_mb: 65536,
                available_mb: 48000,
                speed_mhz: 6000,
                dimm_count: 4,
            },
            pci_devices: Vec::new(),
            storage_devices: Vec::new(),
            usb_devices: Vec::new(),
            thunderbolt_devices: Vec::new(),
        };
        assert_eq!(AiTier::from_profile(&profile), AiTier::Tier2);
    }

    #[test]
    fn test_tier1_requires_avx512() {
        let profile = HardwareProfile {
            cpu: CpuInfo {
                cores: 16,
                threads: 32,
                model: "ARM CPU".into(),
                has_avx512: false,
                has_avx2: false,
                has_sse42: false,
                has_neon: true,
                base_freq_mhz: 3500,
                vendor: CpuVendor::Apple,
            },
            gpu: Some(GpuInfo {
                name: "Apple GPU".into(),
                vram_mb: 18432,
                compute_shaders: true,
                vendor: "Apple".into(),
                driver_version: String::new(),
                cuda_cores: 0,
                compute_capability: String::new(),
            }),
            npu: Some(NpuInfo {
                name: "Apple Neural Engine".into(),
                tops: 18,
                supported_frameworks: vec!["CoreML".into()],
            }),
            memory: MemoryInfo {
                total_mb: 18432,
                available_mb: 12288,
                speed_mhz: 6400,
                dimm_count: 1,
            },
            pci_devices: Vec::new(),
            storage_devices: Vec::new(),
            usb_devices: Vec::new(),
            thunderbolt_devices: Vec::new(),
        };
        assert_eq!(AiTier::from_profile(&profile), AiTier::Tier2);
    }

    #[test]
    fn test_tier_descriptions() {
        assert!(!AiTier::Tier1.description().is_empty());
        assert!(!AiTier::Tier2.description().is_empty());
        assert!(!AiTier::Tier3.description().is_empty());
    }

    #[test]
    fn test_tier_display() {
        assert_eq!(format!("{}", AiTier::Tier1), "Tier 1 (Local LLM)");
        assert_eq!(format!("{}", AiTier::Tier2), "Tier 2 (Quantized SLM)");
        assert_eq!(format!("{}", AiTier::Tier3), "Tier 3 (Heuristic Fallback)");
    }
}
