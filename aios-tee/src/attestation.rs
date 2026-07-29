//! TEE Attestation Framework
//!
//! Provides remote attestation capabilities for TEE enclaves,
//! allowing external verification of enclave integrity and authenticity.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Attestation report containing enclave measurements and signatures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationReport {
    enclave_id: u32,
    mrenclave: Vec<u8>,
    mrsigner: Vec<u8>,
    attributes: u64,
    timestamp: u64,
    quote_body: Vec<u8>,
    signature: Vec<u8>,
}

impl AttestationReport {
    /// Create a new attestation report
    pub fn new(
        enclave_id: u32,
        mrenclave: Vec<u8>,
        mrsigner: Vec<u8>,
        attributes: u64,
        timestamp: u64,
        quote_body: Vec<u8>,
        signature: Vec<u8>,
    ) -> Self {
        Self {
            enclave_id,
            mrenclave,
            mrsigner,
            attributes,
            timestamp,
            quote_body,
            signature,
        }
    }

    /// Get enclave ID
    pub fn enclave_id(&self) -> u32 {
        self.enclave_id
    }

    /// Get MRENCLAVE (enclave measurement)
    pub fn mrenclave(&self) -> &[u8] {
        &self.mrenclave
    }

    /// Get MRSIGNER (signer measurement)
    pub fn mrsigner(&self) -> &[u8] {
        &self.mrsigner
    }

    /// Get enclave attributes
    pub fn attributes(&self) -> u64 {
        self.attributes
    }

    /// Get attestation timestamp
    pub fn timestamp(&self) -> u64 {
        self.timestamp
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

/// Attestation with verification status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attestation {
    report: AttestationReport,
    verified: bool,
    verification_time: u64,
    pcr_values: HashMap<u32, Vec<u8>>,
}

impl Attestation {
    /// Create a new attestation
    pub fn new(report: AttestationReport) -> Self {
        Self {
            report,
            verified: false,
            verification_time: 0,
            pcr_values: HashMap::new(),
        }
    }

    /// Mark attestation as verified
    pub fn mark_verified(&mut self, verification_time: u64) {
        self.verified = true;
        self.verification_time = verification_time;
    }

    /// Check if attestation is verified
    pub fn is_verified(&self) -> bool {
        self.verified
    }

    /// Get verification time
    pub fn verification_time(&self) -> u64 {
        self.verification_time
    }

    /// Get attestation report
    pub fn report(&self) -> &AttestationReport {
        &self.report
    }

    /// Add PCR value (Platform Configuration Register)
    pub fn add_pcr(&mut self, index: u32, value: Vec<u8>) {
        self.pcr_values.insert(index, value);
    }

    /// Get PCR value
    pub fn get_pcr(&self, index: u32) -> Option<&Vec<u8>> {
        self.pcr_values.get(&index)
    }

    /// Compute hash of report for verification
    pub fn compute_report_hash(&self) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(self.report.to_bytes());
        hasher.finalize().to_vec()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attestation_report_creation() {
        let report = AttestationReport::new(
            1,
            vec![1; 32],
            vec![2; 32],
            0x1234,
            1000,
            vec![3; 64],
            vec![4; 64],
        );

        assert_eq!(report.enclave_id(), 1);
        assert_eq!(report.mrenclave().len(), 32);
        assert_eq!(report.mrsigner().len(), 32);
        assert_eq!(report.attributes(), 0x1234);
        assert_eq!(report.timestamp(), 1000);
    }

    #[test]
    fn test_attestation_verification_status() {
        let report = AttestationReport::new(
            1,
            vec![1; 32],
            vec![2; 32],
            0x1234,
            1000,
            vec![3; 64],
            vec![4; 64],
        );

        let mut attestation = Attestation::new(report);
        assert!(!attestation.is_verified());

        attestation.mark_verified(2000);
        assert!(attestation.is_verified());
        assert_eq!(attestation.verification_time(), 2000);
    }

    #[test]
    fn test_attestation_pcr_operations() {
        let report = AttestationReport::new(
            1,
            vec![1; 32],
            vec![2; 32],
            0x1234,
            1000,
            vec![3; 64],
            vec![4; 64],
        );

        let mut attestation = Attestation::new(report);
        attestation.add_pcr(0, vec![5; 20]);
        attestation.add_pcr(1, vec![6; 20]);

        assert_eq!(attestation.get_pcr(0), Some(&vec![5; 20]));
        assert_eq!(attestation.get_pcr(1), Some(&vec![6; 20]));
        assert_eq!(attestation.get_pcr(2), None);
    }

    #[test]
    fn test_attestation_report_serialization() {
        let original = AttestationReport::new(
            1,
            vec![1; 32],
            vec![2; 32],
            0x1234,
            1000,
            vec![3; 64],
            vec![4; 64],
        );

        let bytes = original.to_bytes();
        let recovered = AttestationReport::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.enclave_id(), original.enclave_id());
        assert_eq!(recovered.mrenclave(), original.mrenclave());
        assert_eq!(recovered.mrsigner(), original.mrsigner());
    }

    #[test]
    fn test_attestation_serialization() {
        let report = AttestationReport::new(
            1,
            vec![1; 32],
            vec![2; 32],
            0x1234,
            1000,
            vec![3; 64],
            vec![4; 64],
        );

        let mut attestation = Attestation::new(report);
        attestation.mark_verified(2000);

        let bytes = attestation.to_bytes();
        let recovered = Attestation::from_bytes(&bytes).unwrap();

        assert!(recovered.is_verified());
        assert_eq!(recovered.verification_time(), 2000);
    }

    #[test]
    fn test_attestation_report_hash() {
        let report = AttestationReport::new(
            1,
            vec![1; 32],
            vec![2; 32],
            0x1234,
            1000,
            vec![3; 64],
            vec![4; 64],
        );

        let attestation = Attestation::new(report);
        let hash = attestation.compute_report_hash();
        assert_eq!(hash.len(), 32);

        let hash2 = attestation.compute_report_hash();
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_attestation_report_debug_format() {
        let report = AttestationReport::new(
            1,
            vec![1; 32],
            vec![2; 32],
            0x1234,
            1000,
            vec![3; 64],
            vec![4; 64],
        );

        let debug_str = format!("{:?}", report);
        assert!(debug_str.contains("AttestationReport"));
    }
}
