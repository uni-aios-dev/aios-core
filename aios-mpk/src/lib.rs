pub mod arm_domains;
pub mod integration;
pub mod mpk;

pub use arm_domains::{ArmDomain, ArmDomainCapability, ArmDomainManager};
pub use integration::{MpkBlockPolicy, MpkSecurityBridge};
pub use mpk::{AccessType, MpkCapability, MpkKey, MpkManager, MpkPolicy};

use aios_core::error::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HwProtectionMode {
    Intel,
    Arm,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HwMemoryProtection {
    pub mode: HwProtectionMode,
    pub available: bool,
    pub max_keys: u32,
}

impl HwMemoryProtection {
    pub fn detect() -> Result<Self> {
        #[cfg(target_arch = "x86_64")]
        {
            match mpk::MpkManager::detect() {
                Ok(mgr) => Ok(Self {
                    mode: HwProtectionMode::Intel,
                    available: mgr.supported(),
                    max_keys: 16,
                }),
                Err(_) => Ok(Self {
                    mode: HwProtectionMode::Disabled,
                    available: false,
                    max_keys: 0,
                }),
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            match arm_domains::ArmDomainManager::detect() {
                Ok(mgr) => Ok(Self {
                    mode: HwProtectionMode::Arm,
                    available: mgr.supported(),
                    max_keys: 4,
                }),
                Err(_) => Ok(Self {
                    mode: HwProtectionMode::Disabled,
                    available: false,
                    max_keys: 0,
                }),
            }
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Ok(Self {
                mode: HwProtectionMode::Disabled,
                available: false,
                max_keys: 0,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hw_protection_detection() {
        let result = HwMemoryProtection::detect();
        assert!(result.is_ok());
        let protection = result.unwrap();
        match protection.mode {
            HwProtectionMode::Intel => assert_eq!(protection.max_keys, 16),
            HwProtectionMode::Arm => assert_eq!(protection.max_keys, 4),
            HwProtectionMode::Disabled => assert_eq!(protection.max_keys, 0),
        }
    }
}
