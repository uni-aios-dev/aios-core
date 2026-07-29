use crate::capability::{Capability, CapabilityToken};
use aios_core::error::{AIOSException, Result};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Violation {
    pub block_id: u32,
    pub requested_capability: Capability,
    pub timestamp_ms: u64,
    pub details: String,
}

pub struct AccessControlLayer {
    tokens: HashMap<u32, CapabilityToken>,
    violations: Vec<Violation>,
    issuer_secret: Vec<u8>,
    default_ttl_ms: u64,
}

impl AccessControlLayer {
    pub fn new(issuer_secret: Vec<u8>, default_ttl_ms: u64) -> Self {
        Self {
            tokens: HashMap::new(),
            violations: Vec::new(),
            issuer_secret,
            default_ttl_ms,
        }
    }

    pub fn issue_token(
        &mut self,
        block_id: u32,
        capabilities: Vec<Capability>,
    ) -> Result<&CapabilityToken> {
        let token = CapabilityToken::new(
            block_id,
            capabilities,
            self.default_ttl_ms,
            &self.issuer_secret,
        );
        self.tokens.insert(block_id, token);
        Ok(self.tokens.get(&block_id).unwrap())
    }

    pub fn issue_token_with_ttl(
        &mut self,
        block_id: u32,
        capabilities: Vec<Capability>,
        ttl_ms: u64,
    ) -> Result<&CapabilityToken> {
        let token = CapabilityToken::new(block_id, capabilities, ttl_ms, &self.issuer_secret);
        self.tokens.insert(block_id, token);
        Ok(self.tokens.get(&block_id).unwrap())
    }

    pub fn revoke_token(&mut self, block_id: u32) -> bool {
        self.tokens.remove(&block_id).is_some()
    }

    pub fn check_permission(&self, block_id: u32, required: &Capability) -> Result<()> {
        let token = self.tokens.get(&block_id).ok_or_else(|| {
            AIOSException::PermissionDenied(format!("No token issued for block {block_id}"))
        })?;

        if token.is_expired() {
            return Err(AIOSException::PermissionDenied(format!(
                "Token for block {block_id} has expired"
            )));
        }

        if !token.has_capability(required) {
            return Err(AIOSException::PermissionDenied(format!(
                "Block {block_id} lacks capability {}",
                required.name()
            )));
        }

        Ok(())
    }

    pub fn try_check_permission(&mut self, block_id: u32, required: &Capability) -> bool {
        match self.check_permission(block_id, required) {
            Ok(()) => true,
            Err(_) => {
                self.violations.push(Violation {
                    block_id,
                    requested_capability: *required,
                    timestamp_ms: crate::capability::now_ms(),
                    details: format!("Unauthorized access to {}", required.name()),
                });
                false
            }
        }
    }

    pub fn get_token(&self, block_id: u32) -> Option<&CapabilityToken> {
        self.tokens.get(&block_id)
    }

    pub fn has_token(&self, block_id: u32) -> bool {
        self.tokens.contains_key(&block_id)
    }

    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    pub fn violation_count(&self) -> usize {
        self.violations.len()
    }

    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    pub fn clean_expired(&mut self) -> usize {
        let before = self.tokens.len();
        self.tokens.retain(|_, token| !token.is_expired());
        before - self.tokens.len()
    }

    pub fn issuer_secret(&self) -> &[u8] {
        &self.issuer_secret
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_acl() -> AccessControlLayer {
        AccessControlLayer::new(b"test_secret".to_vec(), 60_000)
    }

    #[test]
    fn test_issue_and_check() {
        let mut acl = test_acl();
        acl.issue_token(1, vec![Capability::FsRead]).unwrap();
        assert!(acl.check_permission(1, &Capability::FsRead).is_ok());
    }

    #[test]
    fn test_check_no_token() {
        let acl = test_acl();
        assert!(acl.check_permission(999, &Capability::FsRead).is_err());
    }

    #[test]
    fn test_check_wrong_capability() {
        let mut acl = test_acl();
        acl.issue_token(1, vec![Capability::FsRead]).unwrap();
        assert!(acl.check_permission(1, &Capability::FsWrite).is_err());
    }

    #[test]
    fn test_revoke_token() {
        let mut acl = test_acl();
        acl.issue_token(1, vec![Capability::FsRead]).unwrap();
        assert!(acl.revoke_token(1));
        assert!(!acl.has_token(1));
    }

    #[test]
    fn test_try_check_records_violation() {
        let mut acl = test_acl();
        assert!(!acl.try_check_permission(1, &Capability::FsRead));
        assert_eq!(acl.violation_count(), 1);
        assert_eq!(acl.violations()[0].block_id, 1);
    }

    #[test]
    fn test_clean_expired() {
        let mut acl = test_acl();
        acl.issue_token(1, vec![Capability::FsRead]).unwrap();
        acl.issue_token_with_ttl(2, vec![Capability::FsRead], 0)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let removed = acl.clean_expired();
        assert!(removed >= 1);
        assert!(!acl.has_token(2));
    }

    #[test]
    fn test_token_count() {
        let mut acl = test_acl();
        acl.issue_token(1, vec![Capability::FsRead]).unwrap();
        acl.issue_token(2, vec![Capability::FsWrite]).unwrap();
        assert_eq!(acl.token_count(), 2);
    }
}
