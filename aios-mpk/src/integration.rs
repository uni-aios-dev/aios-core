use crate::mpk::*;
use aios_core::error::Result;
use aios_security::capability::Capability;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MpkBlockPolicy {
    pub block_id: u32,
    pub assigned_key: Option<u32>,
    pub allowed_capabilities: Vec<Capability>,
}

impl MpkBlockPolicy {
    pub fn new(block_id: u32, capabilities: Vec<Capability>) -> Self {
        Self {
            block_id,
            assigned_key: None,
            allowed_capabilities: capabilities,
        }
    }

    pub fn assign_key(&mut self, key: u32) {
        self.assigned_key = Some(key);
    }

    pub fn can_access(&self, required_capability: &Capability) -> bool {
        self.allowed_capabilities.contains(required_capability)
            || self.allowed_capabilities.contains(&Capability::All)
    }
}

pub struct MpkSecurityBridge {
    mpk_manager: MpkManager,
    policies: Vec<MpkBlockPolicy>,
}

impl MpkSecurityBridge {
    pub fn new() -> Result<Self> {
        let mpk_manager = MpkManager::detect()?;
        Ok(Self {
            mpk_manager,
            policies: Vec::new(),
        })
    }

    pub fn register_block(&mut self, policy: MpkBlockPolicy) -> Result<()> {
        self.policies.push(policy);
        Ok(())
    }

    pub fn enforce_policy(&mut self, block_id: u32) -> Result<()> {
        for policy in &mut self.policies {
            if policy.block_id == block_id
                && policy.assigned_key.is_none()
                && self.mpk_manager.active_keys_count() < 16
            {
                let key = self.mpk_manager.allocate_key()?;
                policy.assign_key(key.index);

                let mut cap = MpkCapability::none();
                if policy.can_access(&Capability::FsRead) {
                    cap.read_allowed = true;
                }
                if policy.can_access(&Capability::FsWrite) {
                    cap.write_allowed = true;
                }
                self.mpk_manager.set_capability(key, cap)?;
            }
        }
        Ok(())
    }

    pub fn verify_access(&self, source_block: u32, target_key: u32, access: AccessType) -> bool {
        self.mpk_manager
            .check_access(source_block, target_key, access)
    }

    pub fn mpk_supported(&self) -> bool {
        self.mpk_manager.supported()
    }

    pub fn active_keys(&self) -> usize {
        self.mpk_manager.active_keys_count()
    }
}

impl Default for MpkSecurityBridge {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self {
            mpk_manager: MpkManager::detect().unwrap(),
            policies: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mpk_block_policy_creation() {
        let policy = MpkBlockPolicy::new(1, vec![Capability::FsRead, Capability::FsWrite]);
        assert_eq!(policy.block_id, 1);
        assert_eq!(policy.allowed_capabilities.len(), 2);
        assert!(policy.assigned_key.is_none());
    }

    #[test]
    fn test_mpk_block_policy_assign_key() {
        let mut policy = MpkBlockPolicy::new(1, vec![Capability::FsRead]);
        policy.assign_key(5);
        assert_eq!(policy.assigned_key, Some(5));
    }

    #[test]
    fn test_mpk_block_policy_can_access() {
        let policy = MpkBlockPolicy::new(1, vec![Capability::FsRead, Capability::FsWrite]);
        assert!(policy.can_access(&Capability::FsRead));
        assert!(policy.can_access(&Capability::FsWrite));
        assert!(!policy.can_access(&Capability::NetBind));
    }

    #[test]
    fn test_mpk_security_bridge_creation() {
        let bridge = MpkSecurityBridge::new();
        assert!(bridge.is_ok());
    }

    #[test]
    fn test_mpk_security_bridge_register_block() {
        let mut bridge = MpkSecurityBridge::new().unwrap();
        let policy = MpkBlockPolicy::new(42, vec![Capability::FsRead]);

        let result = bridge.register_block(policy);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mpk_security_bridge_enforce_policy() {
        let mut bridge = MpkSecurityBridge::new().unwrap();
        let policy = MpkBlockPolicy::new(42, vec![Capability::FsRead]);

        bridge.register_block(policy).unwrap();
        let result = bridge.enforce_policy(42);
        assert!(result.is_ok());
    }
}
