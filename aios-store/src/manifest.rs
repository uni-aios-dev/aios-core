use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

/// Deterministic canonical bytes of a manifest that are covered by the
/// Ed25519 signature. The signature over these bytes binds name, version,
/// description, author, capabilities and the wasm SHA-256, so tampering with
/// any of them invalidates the signature.
pub fn canonical_bytes(manifest: &ManifestInfo) -> Vec<u8> {
    let mut caps: Vec<&str> = manifest.capabilities.iter().map(String::as_str).collect();
    caps.sort_unstable();
    format!(
        "aios-manifest-v1\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        manifest.name,
        manifest.version,
        manifest.description,
        manifest.author,
        caps.join(","),
        manifest.wasm_size_bytes,
        manifest.wasm_sha256,
    )
    .into_bytes()
}

/// Sign a manifest with an Ed25519 signing key, producing the signature block
/// embedded in [`ManifestInfo::signature`]. The manifest's `wasm_sha256` must
/// already match the binary the manifest describes.
pub fn sign_manifest(manifest: &ManifestInfo, signing_key: &SigningKey) -> SignatureInfo {
    let signature = signing_key.sign(&canonical_bytes(manifest));
    SignatureInfo {
        algorithm: "Ed25519".into(),
        public_key: hex::encode(signing_key.verifying_key().to_bytes()),
        signature_hex: hex::encode(signature.to_bytes()),
        signed_hash: manifest.wasm_sha256.clone(),
    }
}

fn decode_public_key(hex_str: &str) -> Result<VerifyingKey, String> {
    let bytes = hex::decode(hex_str).map_err(|e| format!("Invalid public key hex: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "Public key must be 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&arr).map_err(|e| format!("Invalid public key: {e}"))
}

fn decode_signature(hex_str: &str) -> Result<Signature, String> {
    let bytes = hex::decode(hex_str).map_err(|e| format!("Invalid signature hex: {e}"))?;
    let arr: [u8; 64] = bytes
        .try_into()
        .map_err(|_| "Signature must be 64 bytes".to_string())?;
    Ok(Signature::from_bytes(&arr))
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

    /// Cryptographically verify the Ed25519 signature embedded in the manifest,
    /// using the public key that is embedded next to it. `signed_hash` must
    /// equal `wasm_sha256` (the hash of the binary).
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

        let public = decode_public_key(&sig.public_key)?;
        let signature = decode_signature(&sig.signature_hex)?;
        Ok(public
            .verify_strict(&canonical_bytes(manifest), &signature)
            .is_ok())
    }

    /// Verify the manifest signature against a set of trusted public keys.
    /// Succeeds only when the manifest's embedded public key matches one of the
    /// trusted keys and the Ed25519 signature verifies over the canonical
    /// manifest bytes.
    pub fn verify_signature_with_keys(
        manifest: &ManifestInfo,
        trusted_keys: &[String],
    ) -> Result<bool, String> {
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

        let signature = decode_signature(&sig.signature_hex)?;
        let msg = canonical_bytes(manifest);
        for key in trusted_keys {
            if sig.public_key != *key {
                continue;
            }
            let public = decode_public_key(key)?;
            if public.verify_strict(&msg, &signature).is_ok() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;

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

    fn signed_manifest(manifest: &ManifestInfo, key: &SigningKey) -> ManifestInfo {
        let mut m = manifest.clone();
        m.signature = Some(sign_manifest(&m, key));
        m
    }

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

    #[test]
    fn test_sign_and_verify_roundtrip() {
        let key = SigningKey::generate(&mut OsRng);
        let m = signed_manifest(&sample_manifest(), &key);
        assert_eq!(m.signature.as_ref().unwrap().algorithm, "Ed25519");
        assert!(ManifestValidator::verify_signature(&m).unwrap());
    }

    #[test]
    fn test_verify_rejects_tampered_wasm_hash() {
        let key = SigningKey::generate(&mut OsRng);
        let mut m = signed_manifest(&sample_manifest(), &key);
        m.wasm_sha256 = hex::encode(Sha256::digest(b"tampered"));
        assert!(ManifestValidator::verify_signature(&m).is_err());
    }

    #[test]
    fn test_verify_rejects_tampered_capability() {
        let key = SigningKey::generate(&mut OsRng);
        let mut m = signed_manifest(&sample_manifest(), &key);
        m.capabilities.insert("CAP_NET_BIND".into());
        assert!(!ManifestValidator::verify_signature(&m).unwrap());
    }

    #[test]
    fn test_verify_rejects_wrong_key() {
        let signer = SigningKey::generate(&mut OsRng);
        let other = SigningKey::generate(&mut OsRng);
        let m = signed_manifest(&sample_manifest(), &signer);
        let foreign = hex::encode(other.verifying_key().to_bytes());
        assert!(!ManifestValidator::verify_signature_with_keys(&m, &[foreign]).unwrap());
    }

    #[test]
    fn test_verify_with_keys_accepts_trusted() {
        let signer = SigningKey::generate(&mut OsRng);
        let m = signed_manifest(&sample_manifest(), &signer);
        let trusted = hex::encode(signer.verifying_key().to_bytes());
        assert!(ManifestValidator::verify_signature_with_keys(&m, &[trusted]).unwrap());
    }

    #[test]
    fn test_verify_missing_signature_errors() {
        assert!(ManifestValidator::verify_signature(&sample_manifest()).is_err());
    }

    #[test]
    fn test_verify_bad_algorithm_errors() {
        let key = SigningKey::generate(&mut OsRng);
        let mut m = signed_manifest(&sample_manifest(), &key);
        m.signature.as_mut().unwrap().algorithm = "RSA".into();
        assert!(ManifestValidator::verify_signature(&m).is_err());
    }
}
