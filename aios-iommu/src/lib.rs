pub mod dma;
pub mod iommu;
pub mod page_table;

pub use dma::{DmaBuffer, DmaRegion};
pub use iommu::{IommuManager, IommuStatus};
pub use page_table::{PageTable, PageTableEntry};

use aios_core::error::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IommuType {
    IntelVtd,
    AmdIommu,
    ArmSmmu,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IommuInfo {
    pub iommu_type: IommuType,
    pub available: bool,
    pub max_domain_id: u32,
    pub max_devices: u32,
}

impl IommuInfo {
    pub fn detect() -> Result<Self> {
        #[cfg(target_arch = "x86_64")]
        {
            if let Ok(info) = Self::detect_intel_vt_d() {
                return Ok(info);
            }
            if let Ok(info) = Self::detect_amd_iommu() {
                return Ok(info);
            }
        }

        Ok(IommuInfo {
            iommu_type: IommuType::Disabled,
            available: false,
            max_domain_id: 0,
            max_devices: 0,
        })
    }

    fn detect_intel_vt_d() -> Result<Self> {
        Ok(IommuInfo {
            iommu_type: IommuType::IntelVtd,
            available: false,
            max_domain_id: 256,
            max_devices: 65536,
        })
    }

    fn detect_amd_iommu() -> Result<Self> {
        Err(aios_core::error::AIOSException::HardwareNotDetected(
            "AMD IOMMU not detected".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iommu_detection() {
        let result = IommuInfo::detect();
        assert!(result.is_ok());
    }

    #[test]
    fn test_iommu_types() {
        assert_ne!(IommuType::IntelVtd, IommuType::AmdIommu);
        assert_ne!(IommuType::ArmSmmu, IommuType::Disabled);
    }

    #[test]
    fn test_iommu_info_creation() {
        let info = IommuInfo {
            iommu_type: IommuType::IntelVtd,
            available: true,
            max_domain_id: 256,
            max_devices: 65536,
        };
        assert!(info.available);
        assert_eq!(info.max_domain_id, 256);
    }
}
