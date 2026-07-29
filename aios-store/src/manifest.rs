use serde::{Deserialize, Serialize};
use sha2::{ Digest, Sha256};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub capabilities: HashSet<String>,
    pub wasm_size_bytes: u64,
    pub wasm_sha256: String,
    pub signature: Option<SignatureInfo>,
    pub store_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureInfo {
    pub algorithm: String,
    pub public_key: String,
    pub signature_hex: String,
    pub signed_hash: String,
}

pub struct ManifestValidator;

impl ManifestValidator {
    pub fn validate_sha256(manifest: &ManifestInfo, wasm_data: &[u8]) -> Result<bool, String> {
        let hash = hex::encode(Sha256::digest(wasm_data));
        Ok(hash == manifest.wasm_sha256)
    }

    pub fn validate_capabilities(manifest: &ManifestInfo) -> Result<(), String> {
        let valid = [
            "CAP_NET_BIND",
            "CAP_NET_CONNECT",
            "CAP_FS_READ",
            "CAP_FS_WRITE",
            "CAP_HW_ACCESS",
            "CAP_MEM_ALLOC",
            "CAP_SCHED_MODIFY",
            "CAP_BROWSER_RENDER",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<HashSet<_>>();

        for cap in &manifest.capabilities {
            if !valid.contains(cap.as_str()) {
                return Err(format!("Unknown capability: {cap}"));
            }
        }
        Ok(())
    }

    pub fn verify_signature(manifest: &ManifestInfo) -> Result<bool, String> {
        let sig = manifest
            .signature
            .as_ref()
            .ok_or_else(|| "No signature".to_string())?;

        if sig.algorithm != "Ed25519" {
            return Err(format!("Unsupported algorithm: {}", sig.algorithm));
        }

        if sig.signed_hash != manifest.wasm_sha256 {
            return Err("Signature hash mismatch".into());
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_validation_sha256() {
        let data = b"wasm binary content";
        let hash = hex::encode(Sha256::digest(data));
        let manifest = ManifestInfo {
            wasm_sha256: hash.clone(),
            ..sample_manifest()
        };
        assert!(ManifestValidator::validate_sha256(&manifest, data).unwrap());
    }

    #[test]
    fn test_manifest_validation_sha256_fails() {
        let data = b"wasm binary content";
        let manifest = ManifestInfo {
            wasm_sha256: "badhash".into(),
            ..sample_manifest()
        };
        assert!(!ManifestValidator::validate_sha256(&manifest, data).unwrap());
    }

    #[test]
    fn test_capability_validation_valid() {
        let mut m = sample_manifest();
        m.capabilities = ["CAP_NET_CONNECT", "CAP_FS_READ"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(ManifestValidator::validate_capabilities(&m).is_ok());
    }

    #[test]
    fn test_capability_validation_invalid() {
        let mut m = sample_manifest();
        m.capabilities.insert("CAP_INVALID".into());
        assert!(ManifestValidator::validate_capabilities(&m).is_err());
    }

    fn sample_manifest() -> ManifestInfo {
        ManifestInfo {
            name: "test".into(),
            version: "1.0".into(),
            description: "test block".into(),
            author: "test".into(),
            capabilities: HashSet::new(),
            wasm_size_bytes: 100,
            wasm_sha256: hex::encode(Sha256::digest(b"test")),
            signature: None,
            store_url: Some("https://github.com/uni-aios-dev/aios-official-store".into()),
        }
    }
}
