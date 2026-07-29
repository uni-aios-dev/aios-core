use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MpkKey {
    pub index: u32,
}

impl MpkKey {
    pub fn new(index: u32) -> Result<Self> {
        if index >= 16 {
            return Err(AIOSException::HardwareNotDetected(format!(
                "MPK index must be 0-15, got {}",
                index
            )));
        }
        Ok(MpkKey { index })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MpkDomain {
    User,
    Supervisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MpkCapability {
    pub read_allowed: bool,
    pub write_allowed: bool,
    pub execute_allowed: bool,
}

impl MpkCapability {
    pub fn read_only() -> Self {
        Self {
            read_allowed: true,
            write_allowed: false,
            execute_allowed: false,
        }
    }

    pub fn write_only() -> Self {
        Self {
            read_allowed: false,
            write_allowed: true,
            execute_allowed: false,
        }
    }

    pub fn read_write() -> Self {
        Self {
            read_allowed: true,
            write_allowed: true,
            execute_allowed: false,
        }
    }

    pub fn full() -> Self {
        Self {
            read_allowed: true,
            write_allowed: true,
            execute_allowed: true,
        }
    }

    pub fn none() -> Self {
        Self {
            read_allowed: false,
            write_allowed: false,
            execute_allowed: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MpkPolicy {
    pub source_block_id: u32,
    pub target_key: u32,
    pub capabilities: MpkCapability,
}

pub struct MpkManager {
    supported: bool,
    active_keys: HashMap<u32, MpkCapability>,
    policies: Vec<MpkPolicy>,
    current_pkru: u32,
}

impl MpkManager {
    pub fn detect() -> Result<Self> {
        let supported = Self::check_mpk_support();
        Ok(MpkManager {
            supported,
            active_keys: HashMap::new(),
            policies: Vec::new(),
            current_pkru: 0,
        })
    }

    pub fn supported(&self) -> bool {
        self.supported
    }

    fn check_mpk_support() -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            use x86::cpuid::CpuId;
            let cpuid = CpuId::new();
            if let Some(extended_features) = cpuid.get_extended_feature_info() {
                return extended_features.has_pku();
            }
            false
        }

        #[cfg(not(target_arch = "x86_64"))]
        false
    }

    pub fn allocate_key(&mut self) -> Result<MpkKey> {
        for i in 0..16 {
            use std::collections::hash_map::Entry;
            match self.active_keys.entry(i) {
                Entry::Vacant(e) => {
                    let key = MpkKey::new(i)?;
                    e.insert(MpkCapability::none());
                    return Ok(key);
                }
                Entry::Occupied(_) => continue,
            }
        }
        Err(AIOSException::HardwareNotDetected(
            "All MPK keys are already allocated".to_string(),
        ))
    }

    pub fn free_key(&mut self, key: MpkKey) -> Result<()> {
        if self.active_keys.remove(&key.index).is_none() {
            return Err(AIOSException::HardwareNotDetected(format!(
                "Key {} is not allocated",
                key.index
            )));
        }
        self.policies.retain(|p| p.target_key != key.index);
        Ok(())
    }

    pub fn set_capability(&mut self, key: MpkKey, cap: MpkCapability) -> Result<()> {
        if !self.active_keys.contains_key(&key.index) {
            return Err(AIOSException::HardwareNotDetected(format!(
                "Key {} is not allocated",
                key.index
            )));
        }
        self.active_keys.insert(key.index, cap);
        self.update_pkru();
        Ok(())
    }

    pub fn add_policy(&mut self, policy: MpkPolicy) -> Result<()> {
        if !self.active_keys.contains_key(&policy.target_key) {
            return Err(AIOSException::HardwareNotDetected(format!(
                "Target key {} is not allocated",
                policy.target_key
            )));
        }
        self.policies.push(policy);
        Ok(())
    }

    pub fn check_access(
        &self,
        source_block: u32,
        target_key: u32,
        access_type: AccessType,
    ) -> bool {
        for policy in &self.policies {
            if policy.source_block_id == source_block && policy.target_key == target_key {
                return match access_type {
                    AccessType::Read => policy.capabilities.read_allowed,
                    AccessType::Write => policy.capabilities.write_allowed,
                    AccessType::Execute => policy.capabilities.execute_allowed,
                };
            }
        }
        false
    }

    fn update_pkru(&mut self) {
        let mut pkru: u32 = 0;
        for (key_idx, cap) in &self.active_keys {
            let disabled = !cap.read_allowed && !cap.write_allowed && !cap.execute_allowed;
            if disabled {
                pkru |= 3 << (key_idx * 2);
            }
        }
        self.current_pkru = pkru;
    }

    pub fn pkru_value(&self) -> u32 {
        self.current_pkru
    }

    pub fn write_pkru(&self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            let msr_pkrs = 0x0000_06e1u32;
            use x86::msr;
            msr::wrmsr(msr_pkrs, self.current_pkru as u64);
        }
    }

    pub fn read_pkru(&self) -> u32 {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            let msr_pkrs = 0x0000_06e1u32;
            use x86::msr;
            msr::rdmsr(msr_pkrs) as u32
        }
        #[cfg(not(target_arch = "x86_64"))]
        self.current_pkru
    }

    pub fn policies(&self) -> &[MpkPolicy] {
        &self.policies
    }

    pub fn active_keys_count(&self) -> usize {
        self.active_keys.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    Read,
    Write,
    Execute,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mpk_key_creation() {
        for i in 0..16 {
            let key = MpkKey::new(i);
            assert!(key.is_ok());
            assert_eq!(key.unwrap().index, i);
        }

        let invalid_key = MpkKey::new(16);
        assert!(invalid_key.is_err());
    }

    #[test]
    fn test_mpk_capability_presets() {
        let read_only = MpkCapability::read_only();
        assert!(read_only.read_allowed);
        assert!(!read_only.write_allowed);
        assert!(!read_only.execute_allowed);

        let write_only = MpkCapability::write_only();
        assert!(!write_only.read_allowed);
        assert!(write_only.write_allowed);
        assert!(!write_only.execute_allowed);

        let full = MpkCapability::full();
        assert!(full.read_allowed);
        assert!(full.write_allowed);
        assert!(full.execute_allowed);

        let none = MpkCapability::none();
        assert!(!none.read_allowed);
        assert!(!none.write_allowed);
        assert!(!none.execute_allowed);
    }

    #[test]
    fn test_mpk_manager_allocation() {
        let mut manager = MpkManager::detect().unwrap();

        let key1 = manager.allocate_key();
        assert!(key1.is_ok());
        assert_eq!(manager.active_keys_count(), 1);

        let key2 = manager.allocate_key();
        assert!(key2.is_ok());
        assert_eq!(manager.active_keys_count(), 2);

        let same_key1 = key1.unwrap();
        let freed = manager.free_key(same_key1);
        assert!(freed.is_ok());
        assert_eq!(manager.active_keys_count(), 1);
    }

    #[test]
    fn test_mpk_manager_set_capability() {
        let mut manager = MpkManager::detect().unwrap();
        let key = manager.allocate_key().unwrap();

        let cap = MpkCapability::read_write();
        let result = manager.set_capability(key, cap);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mpk_policy_add() {
        let mut manager = MpkManager::detect().unwrap();
        let key = manager.allocate_key().unwrap();

        let policy = MpkPolicy {
            source_block_id: 1,
            target_key: key.index,
            capabilities: MpkCapability::read_write(),
        };

        let result = manager.add_policy(policy);
        assert!(result.is_ok());
        assert_eq!(manager.policies().len(), 1);
    }

    #[test]
    fn test_mpk_check_access() {
        let mut manager = MpkManager::detect().unwrap();
        let key = manager.allocate_key().unwrap();

        let policy = MpkPolicy {
            source_block_id: 42,
            target_key: key.index,
            capabilities: MpkCapability::read_only(),
        };

        manager.add_policy(policy).unwrap();

        assert!(manager.check_access(42, key.index, AccessType::Read));
        assert!(!manager.check_access(42, key.index, AccessType::Write));
        assert!(!manager.check_access(99, key.index, AccessType::Read));
    }

    #[test]
    fn test_mpk_pkru_value() {
        let mut manager = MpkManager::detect().unwrap();
        let key1 = manager.allocate_key().unwrap();
        let key2 = manager.allocate_key().unwrap();

        manager.set_capability(key1, MpkCapability::none()).unwrap();
        manager
            .set_capability(key2, MpkCapability::read_write())
            .unwrap();

        let pkru = manager.pkru_value();
        assert_ne!(pkru, 0);
    }

    #[test]
    fn test_mpk_manager_supports_detection() {
        let manager = MpkManager::detect().unwrap();
        let _supported = manager.supported();
    }

    #[test]
    fn test_mpk_all_keys_allocation() {
        let mut manager = MpkManager::detect().unwrap();

        for _ in 0..16 {
            let key = manager.allocate_key();
            assert!(key.is_ok());
        }

        let overflow_key = manager.allocate_key();
        assert!(overflow_key.is_err());
    }

    #[test]
    fn test_mpk_key_double_free() {
        let mut manager = MpkManager::detect().unwrap();
        let key = manager.allocate_key().unwrap();

        let first_free = manager.free_key(key);
        assert!(first_free.is_ok());

        let double_free = manager.free_key(key);
        assert!(double_free.is_err());
    }

    #[test]
    fn test_mpk_policy_with_invalid_key() {
        let mut manager = MpkManager::detect().unwrap();

        let policy = MpkPolicy {
            source_block_id: 1,
            target_key: 99,
            capabilities: MpkCapability::read_only(),
        };

        let result = manager.add_policy(policy);
        assert!(result.is_err());
    }
}
