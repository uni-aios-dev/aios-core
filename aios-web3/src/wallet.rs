use aios_core::error::{AIOSException, Result};
use k256::ecdsa::SigningKey;
use sha3::{Digest, Keccak256};

/// Local Ethereum account derived from an AIOS-managed secret.
///
/// The private key is never exported. Signing uses EIP-191 (`personal_sign`)
/// and returns a recoverable 65-byte signature.
pub struct Wallet {
    signing_key: SigningKey,
    address: String,
}

impl Wallet {
    /// Derive a wallet from secret seed bytes.
    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        if seed.is_empty() {
            return Err(AIOSException::ConfigurationError(
                "Web3 wallet seed is empty".into(),
            ));
        }
        let material = if seed.len() >= 32 {
            seed[..32].to_vec()
        } else {
            Keccak256::digest(seed).to_vec()
        };
        let secret = k256::SecretKey::from_slice(&material)
            .map_err(|e| AIOSException::ConfigurationError(format!("invalid wallet key: {e}")))?;
        let signing_key = SigningKey::from(secret);
        let address = address_from_key(&signing_key);
        Ok(Self {
            signing_key,
            address,
        })
    }

    /// EIP-55 checksummed hex address of the account.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Sign a UTF-8 message using EIP-191 `personal_sign`.
    pub fn sign_message(&self, message: &str) -> Result<String> {
        let bytes = message.as_bytes();
        let mut prefixed = Vec::with_capacity(bytes.len() + 32);
        prefixed.extend_from_slice(b"\x19Ethereum Signed Message:\n");
        prefixed.extend_from_slice(bytes.len().to_string().as_bytes());
        prefixed.extend_from_slice(bytes);
        let (signature, recovery_id) = self
            .signing_key
            .sign_digest_recoverable(Keccak256::new_with_prefix(prefixed));
        let bytes = signature.to_bytes();
        let mut out = Vec::with_capacity(65);
        out.extend_from_slice(&bytes);
        out.push(recovery_id.to_byte());
        Ok(format!("0x{}", hex::encode(out)))
    }
}

/// Derive the EIP-55 address from a secp256k1 signing key.
fn address_from_key(key: &SigningKey) -> String {
    let point = key.verifying_key().to_sec1_point(false);
    let digest = Keccak256::digest(&point.as_bytes()[1..]);
    let lower = hex::encode(&digest[digest.len() - 20..]);
    checksum_address(&lower)
}

/// Apply the EIP-55 mixed-case checksum to a lowercase hex address.
fn checksum_address(lower: &str) -> String {
    let hash = hex::encode(Keccak256::digest(lower.as_bytes()));
    let mut out = String::with_capacity(lower.len() + 2);
    out.push_str("0x");
    for (index, character) in lower.chars().enumerate() {
        if character.is_ascii_digit() {
            out.push(character);
            continue;
        }
        let nibble = hash.as_bytes()[index];
        if nibble >= b'8' {
            out.push(character.to_ascii_uppercase());
        } else {
            out.push(character);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{checksum_address, Wallet};

    #[test]
    fn known_private_key_derives_expected_address() {
        let seed = hex::decode("4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318")
            .expect("valid hex");
        let wallet = Wallet::from_seed(&seed).expect("wallet");
        assert_eq!(
            wallet.address(),
            "0x2c7536E3605D9C16a7a3D7b1898e529396a65c23"
        );
    }

    #[test]
    fn signature_has_recoverable_length() {
        let wallet = Wallet::from_seed(b"aios-web3-test-seed").expect("wallet");
        let signature = wallet.sign_message("hello").expect("signature");
        assert_eq!(signature.len(), 132);
        assert!(signature.starts_with("0x"));
    }

    #[test]
    fn checksum_keeps_digits_unchanged() {
        assert_eq!(
            checksum_address("52908400098527886e0f7030069857d2e4169ee7"),
            "0x52908400098527886E0F7030069857D2E4169EE7"
        );
    }
}
