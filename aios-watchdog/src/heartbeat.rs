use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Heartbeat {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub source_hmac: [u8; 32],
}

impl Heartbeat {
    pub fn new(sequence: u64, secret: &[u8]) -> Self {
        let timestamp_ms = Self::now_ms();
        let source_hmac = Self::compute_hmac(sequence, timestamp_ms, secret);
        Self {
            sequence,
            timestamp_ms,
            source_hmac,
        }
    }

    pub fn verify(&self, secret: &[u8]) -> bool {
        let expected = Self::compute_hmac(self.sequence, self.timestamp_ms, secret);
        self.source_hmac == expected
    }

    pub fn compute_hmac(sequence: u64, timestamp_ms: u64, secret: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(secret);
        hasher.update(sequence.to_le_bytes());
        hasher.update(timestamp_ms.to_le_bytes());
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    pub fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    pub fn age_ms(&self) -> u64 {
        Self::now_ms().saturating_sub(self.timestamp_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heartbeat_create_and_verify() {
        let secret = b"test_secret_key";
        let hb = Heartbeat::new(1, secret);
        assert_eq!(hb.sequence, 1);
        assert!(hb.verify(secret));
    }

    #[test]
    fn test_heartbeat_wrong_secret() {
        let hb = Heartbeat::new(1, b"correct_secret");
        assert!(!hb.verify(b"wrong_secret"));
    }

    #[test]
    fn test_heartbeat_detects_tamper() {
        let secret = b"secret";
        let mut hb = Heartbeat::new(42, secret);
        hb.sequence = 99;
        assert!(!hb.verify(secret));
    }

    #[test]
    fn test_heartbeat_age() {
        let secret = b"secret";
        let mut hb = Heartbeat::new(1, secret);
        hb.timestamp_ms = Heartbeat::now_ms().saturating_sub(5000);
        assert!(hb.age_ms() >= 4900);
    }
}
