use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArmDomain {
    pub index: u32,
}

impl ArmDomain {
    pub fn new(index: u32) -> Result<Self> {
        if index >= 4 {
            return Err(AIOSException::HardwareNotDetected(format!(
                "ARM domain index must be 0-3, got {}",
                index
            )));
        }
        Ok(ArmDomain { index })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmDomainCapability {
    pub access_allowed: bool,
    pub priority: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmDomainPolicy {
    pub source_block_id: u32,
    pub target_domain: u32,
    pub capability: ArmDomainCapability,
}

pub struct ArmDomainManager {
    supported: bool,
    active_domains: HashMap<u32, ArmDomainCapability>,
    policies: Vec<ArmDomainPolicy>,
    current_dacr: u32,
}

impl ArmDomainManager {
    pub fn detect() -> Result<Self> {
        let supported = Self::check_arm_support();
        Ok(ArmDomainManager {
            supported,
            active_domains: HashMap::new(),
            policies: Vec::new(),
            current_dacr: 0,
        })
    }

    pub fn supported(&self) -> bool {
        self.supported
    }

    fn check_arm_support() -> bool {
        #[cfg(target_arch = "aarch64")]
        {
            true
        }
        #[cfg(not(target_arch = "aarch64"))]
        false
    }

    pub fn allocate_domain(&mut self) -> Result<ArmDomain> {
        for i in 0..4 {
            use std::collections::hash_map::Entry;
            match self.active_domains.entry(i) {
                Entry::Vacant(e) => {
                    let domain = ArmDomain::new(i)?;
                    e.insert(ArmDomainCapability {
                        access_allowed: false,
                        priority: 0,
                    });
                    return Ok(domain);
                }
                Entry::Occupied(_) => continue,
            }
        }
        Err(AIOSException::HardwareNotDetected(
            "All ARM domains are already allocated".to_string(),
        ))
    }

    pub fn free_domain(&mut self, domain: ArmDomain) -> Result<()> {
        if self.active_domains.remove(&domain.index).is_none() {
            return Err(AIOSException::HardwareNotDetected(format!(
                "Domain {} is not allocated",
                domain.index
            )));
        }
        self.policies.retain(|p| p.target_domain != domain.index);
        Ok(())
    }

    pub fn set_capability(
        &mut self,
        domain: ArmDomain,
        capability: ArmDomainCapability,
    ) -> Result<()> {
        if !self.active_domains.contains_key(&domain.index) {
            return Err(AIOSException::HardwareNotDetected(format!(
                "Domain {} is not allocated",
                domain.index
            )));
        }
        self.active_domains.insert(domain.index, capability);
        self.update_dacr();
        Ok(())
    }

    pub fn add_policy(&mut self, policy: ArmDomainPolicy) -> Result<()> {
        if !self.active_domains.contains_key(&policy.target_domain) {
            return Err(AIOSException::HardwareNotDetected(format!(
                "Target domain {} is not allocated",
                policy.target_domain
            )));
        }
        self.policies.push(policy);
        Ok(())
    }

    pub fn check_access(&self, source_block: u32, target_domain: u32) -> bool {
        for policy in &self.policies {
            if policy.source_block_id == source_block && policy.target_domain == target_domain {
                return policy.capability.access_allowed;
            }
        }
        false
    }

    fn update_dacr(&mut self) {
        let mut dacr: u32 = 0;
        for (domain_idx, capability) in &self.active_domains {
            if capability.access_allowed {
                dacr |= 1 << (domain_idx * 2);
            }
        }
        self.current_dacr = dacr;
    }

    pub fn dacr_value(&self) -> u32 {
        self.current_dacr
    }

    pub fn write_dacr(&self) {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            std::arch::asm!("msr DACR_EL1, {}", in(reg) self.current_dacr);
        }
    }

    pub fn policies(&self) -> &[ArmDomainPolicy] {
        &self.policies
    }

    pub fn active_domains_count(&self) -> usize {
        self.active_domains.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arm_domain_creation() {
        for i in 0..4 {
            let domain = ArmDomain::new(i);
            assert!(domain.is_ok());
            assert_eq!(domain.unwrap().index, i);
        }

        let invalid_domain = ArmDomain::new(4);
        assert!(invalid_domain.is_err());
    }

    #[test]
    fn test_arm_domain_capability() {
        let cap_allowed = ArmDomainCapability {
            access_allowed: true,
            priority: 5,
        };
        assert!(cap_allowed.access_allowed);
        assert_eq!(cap_allowed.priority, 5);

        let cap_denied = ArmDomainCapability {
            access_allowed: false,
            priority: 0,
        };
        assert!(!cap_denied.access_allowed);
    }

    #[test]
    fn test_arm_domain_manager_allocation() {
        let mut manager = ArmDomainManager::detect().unwrap();

        let domain1 = manager.allocate_domain();
        assert!(domain1.is_ok());
        assert_eq!(manager.active_domains_count(), 1);

        let domain2 = manager.allocate_domain();
        assert!(domain2.is_ok());
        assert_eq!(manager.active_domains_count(), 2);

        let same_domain1 = domain1.unwrap();
        let freed = manager.free_domain(same_domain1);
        assert!(freed.is_ok());
        assert_eq!(manager.active_domains_count(), 1);
    }

    #[test]
    fn test_arm_domain_set_capability() {
        let mut manager = ArmDomainManager::detect().unwrap();
        let domain = manager.allocate_domain().unwrap();

        let cap = ArmDomainCapability {
            access_allowed: true,
            priority: 7,
        };
        let result = manager.set_capability(domain, cap);
        assert!(result.is_ok());
    }

    #[test]
    fn test_arm_domain_policy_add() {
        let mut manager = ArmDomainManager::detect().unwrap();
        let domain = manager.allocate_domain().unwrap();

        let policy = ArmDomainPolicy {
            source_block_id: 1,
            target_domain: domain.index,
            capability: ArmDomainCapability {
                access_allowed: true,
                priority: 3,
            },
        };

        let result = manager.add_policy(policy);
        assert!(result.is_ok());
        assert_eq!(manager.policies().len(), 1);
    }

    #[test]
    fn test_arm_domain_check_access() {
        let mut manager = ArmDomainManager::detect().unwrap();
        let domain = manager.allocate_domain().unwrap();

        let policy = ArmDomainPolicy {
            source_block_id: 42,
            target_domain: domain.index,
            capability: ArmDomainCapability {
                access_allowed: true,
                priority: 5,
            },
        };

        manager.add_policy(policy).unwrap();

        assert!(manager.check_access(42, domain.index));
        assert!(!manager.check_access(99, domain.index));
    }

    #[test]
    fn test_arm_domain_all_allocation() {
        let mut manager = ArmDomainManager::detect().unwrap();

        for _ in 0..4 {
            let domain = manager.allocate_domain();
            assert!(domain.is_ok());
        }

        let overflow_domain = manager.allocate_domain();
        assert!(overflow_domain.is_err());
    }

    #[test]
    fn test_arm_domain_double_free() {
        let mut manager = ArmDomainManager::detect().unwrap();
        let domain = manager.allocate_domain().unwrap();

        let first_free = manager.free_domain(domain);
        assert!(first_free.is_ok());

        let double_free = manager.free_domain(domain);
        assert!(double_free.is_err());
    }

    #[test]
    fn test_arm_domain_dacr_value() {
        let mut manager = ArmDomainManager::detect().unwrap();
        let domain1 = manager.allocate_domain().unwrap();
        let domain2 = manager.allocate_domain().unwrap();

        manager
            .set_capability(
                domain1,
                ArmDomainCapability {
                    access_allowed: true,
                    priority: 1,
                },
            )
            .unwrap();
        manager
            .set_capability(
                domain2,
                ArmDomainCapability {
                    access_allowed: false,
                    priority: 2,
                },
            )
            .unwrap();

        let dacr = manager.dacr_value();
        assert_ne!(dacr, 0);
    }
}
