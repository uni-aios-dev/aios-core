use sha2::{Digest, Sha256};

pub fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

pub fn compute_sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

pub fn verify_sha256(data: &[u8], expected: &str) -> bool {
    compute_sha256(data) == expected
}

pub fn verify_sha256_bytes(data: &[u8], expected: &[u8; 32]) -> bool {
    compute_sha256_bytes(data) == *expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_sha256() {
        let hash = compute_sha256(b"hello world");
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_compute_sha256_bytes() {
        let hash = compute_sha256_bytes(b"hello world");
        assert_eq!(
            hash,
            [
                0xb9, 0x4d, 0x27, 0xb9, 0x93, 0x4d, 0x3e, 0x08, 0xa5, 0x2e, 0x52, 0xd7, 0xda, 0x7d,
                0xab, 0xfa, 0xc4, 0x84, 0xef, 0xe3, 0x7a, 0x53, 0x80, 0xee, 0x90, 0x88, 0xf7, 0xac,
                0xe2, 0xef, 0xcd, 0xe9,
            ]
        );
    }

    #[test]
    fn test_verify_sha256_bytes() {
        let data = b"test data";
        let hash = compute_sha256_bytes(data);
        assert!(verify_sha256_bytes(data, &hash));
        let bad = [0u8; 32];
        assert!(!verify_sha256_bytes(data, &bad));
    }

    #[test]
    fn test_empty_data() {
        let hash = compute_sha256(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
