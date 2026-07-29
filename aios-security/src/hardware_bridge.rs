use crate::capability::Capability;
use aios_core::error::{AIOSException, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareProtectionStatus {
    MpkActive { pkey: u16, block_id: u32 },
    TeeActive { enclave_id: String, block_id: u32 },
    IommuActive { domain_id: u32, block_id: u32 },
    FallbackSoftware,
}

pub struct HardwareSecurityBridge {
    status: Vec<HardwareProtectionStatus>,
}

impl HardwareSecurityBridge {
    pub fn new() -> Self {
        Self { status: Vec::new() }
    }

    pub fn assign_mpk_protection(&mut self, block_id: u32, pkey: u16) -> Result<()> {
        self.status
            .push(HardwareProtectionStatus::MpkActive { pkey, block_id });
        log::info!(
            "SecurityBridge: assigned MPK pkey={} to block_{}",
            pkey,
            block_id
        );
        Ok(())
    }

    pub fn assign_tee_protection(&mut self, block_id: u32, enclave_id: String) -> Result<()> {
        self.status.push(HardwareProtectionStatus::TeeActive {
            enclave_id,
            block_id,
        });
        log::info!("SecurityBridge: assigned TEE enclave to block_{}", block_id);
        Ok(())
    }

    pub fn assign_iommu_protection(&mut self, block_id: u32, domain_id: u32) -> Result<()> {
        self.status.push(HardwareProtectionStatus::IommuActive {
            domain_id,
            block_id,
        });
        log::info!(
            "SecurityBridge: assigned IOMMU domain={} to block_{}",
            domain_id,
            block_id
        );
        Ok(())
    }

    pub fn set_fallback_software(&mut self) {
        self.status.push(HardwareProtectionStatus::FallbackSoftware);
    }

    pub fn protection_for_block(&self, block_id: u32) -> Option<&HardwareProtectionStatus> {
        self.status.iter().find(|s| match s {
            HardwareProtectionStatus::MpkActive { block_id: bid, .. } => *bid == block_id,
            HardwareProtectionStatus::TeeActive { block_id: bid, .. } => *bid == block_id,
            HardwareProtectionStatus::IommuActive { block_id: bid, .. } => *bid == block_id,
            HardwareProtectionStatus::FallbackSoftware => false,
        })
    }

    pub fn validate_hardware_access(
        &self,
        block_id: u32,
        _required_cap: &Capability,
    ) -> Result<()> {
        if self.protection_for_block(block_id).is_some() {
            Ok(())
        } else {
            Err(AIOSException::PermissionDenied(format!(
                "block_{} has no hardware protection assigned",
                block_id
            )))
        }
    }

    pub fn is_hardware_protected(&self, block_id: u32) -> bool {
        self.protection_for_block(block_id).is_some()
    }

    pub fn has_any_hardware_protection(&self) -> bool {
        self.status
            .iter()
            .any(|s| *s != HardwareProtectionStatus::FallbackSoftware)
    }

    pub fn active_protections(&self) -> &[HardwareProtectionStatus] {
        &self.status
    }

    pub fn protection_count(&self) -> usize {
        self.status.len()
    }

    pub fn remove_protection(&mut self, block_id: u32) -> bool {
        let len_before = self.status.len();
        self.status.retain(|s| match s {
            HardwareProtectionStatus::MpkActive { block_id: bid, .. } => *bid != block_id,
            HardwareProtectionStatus::TeeActive { block_id: bid, .. } => *bid != block_id,
            HardwareProtectionStatus::IommuActive { block_id: bid, .. } => *bid != block_id,
            HardwareProtectionStatus::FallbackSoftware => true,
        });
        self.status.len() < len_before
    }

    pub fn clear(&mut self) {
        self.status.clear();
    }
}

impl Default for HardwareSecurityBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mpk_protection() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.assign_mpk_protection(1, 5).unwrap();
        assert!(bridge.is_hardware_protected(1));
        assert!(!bridge.is_hardware_protected(2));
    }

    #[test]
    fn test_tee_protection() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge
            .assign_tee_protection(1, "enclave_abc".into())
            .unwrap();
        assert!(bridge.is_hardware_protected(1));
    }

    #[test]
    fn test_iommu_protection() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.assign_iommu_protection(1, 42).unwrap();
        assert!(bridge.is_hardware_protected(1));
    }

    #[test]
    fn test_fallback_software() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.set_fallback_software();
        assert!(!bridge.has_any_hardware_protection());
    }

    #[test]
    fn test_validate_hardware_access() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.assign_mpk_protection(1, 5).unwrap();
        assert!(bridge
            .validate_hardware_access(1, &Capability::FsRead)
            .is_ok());
        assert!(bridge
            .validate_hardware_access(2, &Capability::FsRead)
            .is_err());
    }

    #[test]
    fn test_remove_protection() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.assign_mpk_protection(1, 5).unwrap();
        bridge.assign_tee_protection(2, "e1".into()).unwrap();
        assert!(bridge.remove_protection(1));
        assert!(!bridge.is_hardware_protected(1));
        assert!(bridge.is_hardware_protected(2));
    }

    #[test]
    fn test_clear() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.assign_mpk_protection(1, 5).unwrap();
        bridge.assign_tee_protection(2, "e1".into()).unwrap();
        bridge.clear();
        assert_eq!(bridge.protection_count(), 0);
    }

    #[test]
    fn test_active_protections() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.assign_mpk_protection(1, 5).unwrap();
        bridge.assign_iommu_protection(2, 10).unwrap();
        assert_eq!(bridge.active_protections().len(), 2);
    }

    #[test]
    fn test_protection_for_block() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.assign_mpk_protection(1, 5).unwrap();
        bridge.assign_iommu_protection(2, 10).unwrap();
        let p = bridge.protection_for_block(1).unwrap();
        assert_eq!(
            *p,
            HardwareProtectionStatus::MpkActive {
                pkey: 5,
                block_id: 1
            }
        );
    }

    #[test]
    fn test_mixed_protections() {
        let mut bridge = HardwareSecurityBridge::new();
        bridge.assign_mpk_protection(1, 5).unwrap();
        bridge
            .assign_tee_protection(2, "enclave_xyz".into())
            .unwrap();
        bridge.assign_iommu_protection(3, 42).unwrap();
        assert!(bridge.has_any_hardware_protection());
        assert_eq!(bridge.protection_count(), 3);
    }
}
