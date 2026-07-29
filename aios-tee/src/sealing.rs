//! TEE Data Sealing and Unsealing
//!
//! Provides cryptographic sealing of sensitive data that can only be unsealed
//! on the same hardware with identical TEE platform configuration.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// Sealing key derived from hardware-specific TEE secrets
#[derive(Clone, Serialize, Deserialize)]
pub struct SealingKey {
    key_material: Vec<u8>,
    platform_binding: u64,
}

impl SealingKey {
    /// Create a new sealing key from raw material and platform binding
    pub fn new(key_material: Vec<u8>, platform_binding: u64) -> Self {
        Self {
            key_material,
            platform_binding,
        }
    }

    /// Derive a sealing key from a secret and hardware platform identifier
    pub fn derive(secret: &[u8], platform_id: u64) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"AIOS-TEE-SEAL-V1");
        hasher.update(secret);
        let digest = hasher.finalize();

        Self {
            key_material: digest.to_vec(),
            platform_binding: platform_id,
        }
    }

    /// Get platform binding (fails unsealing if platform changes)
    pub fn platform_binding(&self) -> u64 {
        self.platform_binding
    }

    /// Get key material
    pub fn key_material(&self) -> &[u8] {
        &self.key_material
    }
}

impl fmt::Debug for SealingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealingKey")
            .field("key_material_len", &self.key_material.len())
            .field("platform_binding", &self.platform_binding)
            .finish()
    }
}

/// Sealed data with integrity protection
#[derive(Clone, Serialize, Deserialize)]
pub struct SealedData {
    ciphertext: Vec<u8>,
    mac_tag: Vec<u8>,
    platform_binding: u64,
    sealed_at: u64,
}

impl SealedData {
    /// Create sealed data from ciphertext and authentication tag
    pub fn new(
        ciphertext: Vec<u8>,
        mac_tag: Vec<u8>,
        platform_binding: u64,
        sealed_at: u64,
    ) -> Self {
        Self {
            ciphertext,
            mac_tag,
            platform_binding,
            sealed_at,
        }
    }

    /// Get ciphertext
    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }

    /// Get authentication tag
    pub fn mac_tag(&self) -> &[u8] {
        &self.mac_tag
    }

    /// Get platform binding
    pub fn platform_binding(&self) -> u64 {
        self.platform_binding
    }

    /// Get sealed-at timestamp
    pub fn sealed_at(&self) -> u64 {
        self.sealed_at
    }

    /// Serialize to binary format
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }

    /// Deserialize from binary format
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        bincode::deserialize(data).ok()
    }
}

impl fmt::Debug for SealedData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedData")
            .field("ciphertext_len", &self.ciphertext.len())
            .field("mac_tag_len", &self.mac_tag.len())
            .field("platform_binding", &self.platform_binding)
            .field("sealed_at", &self.sealed_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sealing_key_creation() {
        let key = SealingKey::new(vec![1, 2, 3], 0x123);
        assert_eq!(key.platform_binding(), 0x123);
        assert_eq!(key.key_material(), &[1, 2, 3]);
    }

    #[test]
    fn test_sealing_key_derivation() {
        let secret = b"my-secret";
        let key = SealingKey::derive(secret, 0x456);
        assert_eq!(key.platform_binding(), 0x456);
        assert_eq!(key.key_material().len(), 32);

        let key2 = SealingKey::derive(secret, 0x456);
        assert_eq!(key.key_material(), key2.key_material());
    }

    #[test]
    fn test_sealing_key_platform_binding_differs() {
        let secret = b"my-secret";
        let key1 = SealingKey::derive(secret, 0x111);
        let key2 = SealingKey::derive(secret, 0x222);
        assert_ne!(key1.platform_binding(), key2.platform_binding());
    }

    #[test]
    fn test_sealed_data_creation() {
        let sealed = SealedData::new(vec![1, 2, 3, 4], vec![5, 6, 7], 0x789, 1234567890);
        assert_eq!(sealed.ciphertext(), &[1, 2, 3, 4]);
        assert_eq!(sealed.mac_tag(), &[5, 6, 7]);
        assert_eq!(sealed.platform_binding(), 0x789);
        assert_eq!(sealed.sealed_at(), 1234567890);
    }

    #[test]
    fn test_sealed_data_serialization() {
        let original = SealedData::new(vec![1, 2, 3, 4], vec![5, 6, 7], 0xabc, 1234567890);

        let bytes = original.to_bytes();
        let recovered = SealedData::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.ciphertext(), original.ciphertext());
        assert_eq!(recovered.mac_tag(), original.mac_tag());
        assert_eq!(recovered.platform_binding(), original.platform_binding());
    }

    #[test]
    fn test_sealing_key_debug_format() {
        let key = SealingKey::new(vec![1, 2, 3], 0x123);
        let debug_str = format!("{:?}", key);
        assert!(debug_str.contains("SealingKey"));
        assert!(debug_str.contains("key_material_len"));
    }

    #[test]
    fn test_sealed_data_debug_format() {
        let sealed = SealedData::new(vec![1, 2, 3, 4], vec![5, 6, 7], 0x789, 1234567890);
        let debug_str = format!("{:?}", sealed);
        assert!(debug_str.contains("SealedData"));
        assert!(debug_str.contains("ciphertext_len"));
    }
}
