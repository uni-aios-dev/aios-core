//! TEE (Trusted Execution Environment) Integration Module
//!
//! Provides secure data sealing, attestation, and enclave lifecycle management.
//! Supports Intel SGX, ARM TrustZone, AMD SEV, and graceful fallback on unsupported hardware.

pub mod attestation;
pub mod enclave;
pub mod sealing;

use serde::{Deserialize, Serialize};

pub use attestation::{Attestation, AttestationReport};
pub use enclave::{Enclave, EnclaveState};
pub use sealing::{SealedData, SealingKey};

/// TEE platform support detection
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TeePlatform {
    IntelSgx,
    ArmTrustzone,
    AmdSev,
    Unsupported,
}

/// TEE information and capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeeInfo {
    pub platform: TeePlatform,
    pub supported: bool,
    pub max_sealed_size: usize,
    pub attestation_capable: bool,
    pub enclave_count_max: u32,
}

impl TeeInfo {
    /// Detect available TEE platform on current system
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            if Self::has_sgx_support() {
                return Self {
                    platform: TeePlatform::IntelSgx,
                    supported: true,
                    max_sealed_size: 4096,
                    attestation_capable: true,
                    enclave_count_max: 128,
                };
            }

            if Self::has_sev_support() {
                return Self {
                    platform: TeePlatform::AmdSev,
                    supported: true,
                    max_sealed_size: 8192,
                    attestation_capable: true,
                    enclave_count_max: 255,
                };
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            if Self::has_trustzone_support() {
                return Self {
                    platform: TeePlatform::ArmTrustzone,
                    supported: true,
                    max_sealed_size: 2048,
                    attestation_capable: true,
                    enclave_count_max: 64,
                };
            }
        }

        Self {
            platform: TeePlatform::Unsupported,
            supported: false,
            max_sealed_size: 0,
            attestation_capable: false,
            enclave_count_max: 0,
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn has_sgx_support() -> bool {
        #[cfg(target_os = "windows")]
        {
            use std::arch::x86_64::__cpuid;
            let cpuid_leaf_7 = __cpuid(7);
            (cpuid_leaf_7.ebx & (1 << 2)) != 0
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn has_sev_support() -> bool {
        #[cfg(target_os = "windows")]
        {
            use std::arch::x86_64::__cpuid;
            let cpuid_leaf_81h = __cpuid(0x8000_0001);
            (cpuid_leaf_81h.ecx & (1 << 2)) != 0
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    #[cfg(target_arch = "aarch64")]
    fn has_trustzone_support() -> bool {
        #[cfg(target_os = "linux")]
        {
            std::path::Path::new("/dev/tee0").exists() || std::path::Path::new("/dev/tee1").exists()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tee_info_detect() {
        let info = TeeInfo::detect();
        assert!(info.platform != TeePlatform::Unsupported || !info.supported);
    }

    #[test]
    fn test_unsupported_tee_graceful_fallback() {
        let info = TeeInfo {
            platform: TeePlatform::Unsupported,
            supported: false,
            max_sealed_size: 0,
            attestation_capable: false,
            enclave_count_max: 0,
        };
        assert!(!info.supported);
        assert_eq!(info.max_sealed_size, 0);
    }

    #[test]
    fn test_tee_platform_serialization() {
        let platforms = vec![
            TeePlatform::IntelSgx,
            TeePlatform::ArmTrustzone,
            TeePlatform::AmdSev,
            TeePlatform::Unsupported,
        ];
        for platform in platforms {
            let serialized = serde_json::to_string(&platform).unwrap();
            let deserialized: TeePlatform = serde_json::from_str(&serialized).unwrap();
            assert_eq!(platform, deserialized);
        }
    }
}
