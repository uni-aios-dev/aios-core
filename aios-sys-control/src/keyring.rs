//! Master encrypted vault for secrets (API keys, tokens, WPA passphrases).
//!
//! Secrets are sealed with AES-256-GCM and persisted in a local redb
//! database. The vault is unlocked either through the hardware TEE/TPM2
//! platform binding ([`aios_tee::sealing::SealingKey`] derived from the
//! detected platform) or, as a fallback, through a PBKDF2-HMAC-SHA256
//! stretched master password. A canary record verifies the password on
//! every unlock so wrong credentials fail at open time instead of first
//! read.
//!
//! Storage layout per secret value: `[12-byte nonce][AES-256-GCM ciphertext]`.
//! Metadata rows (`salt`, `policy`, canary) live in a separate table.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand::RngCore;
use redb::{Database, ReadableTable, TableDefinition};
use sha2::{Digest, Sha256};

use aios_core::error::{AIOSException, Result};
use aios_tee::sealing::SealingKey;

const SECRETS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("secrets");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const CANARY_KEY: &str = "__canary__";
const KDF_ROUNDS: u32 = 120_000;
const KEYRING_DOMAIN: &[u8] = b"aios-keyring-v1";

/// How the vault derives its AES-256 master key.
#[derive(Clone)]
pub enum UnlockPolicy {
    /// Human-supplied master password stretched with PBKDF2 (fallback path).
    MasterPassword(String),
    /// Hardware binding: key material derived from the TEE/TPM2 platform id.
    TeePlatform { platform_id: u64 },
}

impl std::fmt::Debug for UnlockPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MasterPassword(_) => f.write_str("MasterPassword(****)"),
            Self::TeePlatform { platform_id } => {
                f.write_fmt(format_args!("TeePlatform({platform_id})"))
            }
        }
    }
}

impl UnlockPolicy {
    fn tag(&self) -> String {
        match self {
            Self::MasterPassword(_) => "pw".into(),
            Self::TeePlatform { platform_id } => format!("tee:{platform_id}"),
        }
    }
}

fn derive_key(policy: &UnlockPolicy, salt: &[u8]) -> [u8; 32] {
    match policy {
        UnlockPolicy::MasterPassword(pw) => {
            let mut out = [0u8; 32];
            pbkdf2::pbkdf2_hmac::<Sha256>(pw.as_bytes(), salt, KDF_ROUNDS, &mut out);
            out
        }
        UnlockPolicy::TeePlatform { platform_id } => {
            let sealing = SealingKey::derive(KEYRING_DOMAIN, *platform_id);
            let mut hasher = Sha256::new();
            hasher.update(sealing.key_material());
            hasher.update(salt);
            hasher.finalize().into()
        }
    }
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut v);
    v
}

fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| AIOSException::ConfigurationError(format!("cipher init: {e}")))?;
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| AIOSException::InvalidPayload("aes-gcm seal failed".into()))?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ct);
    Ok(blob)
}

fn unseal(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 13 {
        return Err(AIOSException::InvalidPayload("ciphertext too short".into()));
    }
    let (nonce, ct) = blob.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| AIOSException::ConfigurationError(format!("cipher init: {e}")))?;
    cipher.decrypt(Nonce::from_slice(nonce), ct).map_err(|_| {
        AIOSException::PermissionDenied("decryption failed (wrong key or corrupted data)".into())
    })
}

/// Opened vault handle bound to one database file and one unlocked key.
pub struct KeyringVault {
    db: Database,
    key: [u8; 32],
}

impl std::fmt::Debug for KeyringVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringVault").finish_non_exhaustive()
    }
}

impl KeyringVault {
    /// Create (first run) or unlock an existing vault at `path`.
    pub fn open(path: impl AsRef<std::path::Path>, policy: &UnlockPolicy) -> Result<Self> {
        let db = Database::create(path).map_err(db_err)?;
        let existing_salt: Option<Vec<u8>> = match db.begin_read() {
            Ok(tx) => {
                let raw = tx
                    .open_table(META_TABLE)
                    .ok()
                    .and_then(|table| table.get("salt").ok().flatten())
                    .map(|guard| guard.value().to_vec());
                raw.as_deref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .and_then(hex_decode)
            }
            Err(_) => None,
        };

        let (salt, key, fresh) = match existing_salt {
            Some(salt) => {
                let key = derive_key(policy, &salt);
                Self::verify_existing(&db, &key, policy)?;
                (salt, key, false)
            }
            None => {
                let salt = random_bytes(16);
                let key = derive_key(policy, &salt);
                (salt, key, true)
            }
        };

        if fresh {
            Self::init_meta(&db, &salt, policy, &key)?;
        }

        Ok(Self { db, key })
    }

    fn init_meta(db: &Database, salt: &[u8], policy: &UnlockPolicy, key: &[u8; 32]) -> Result<()> {
        let tx = db.begin_write().map_err(db_err)?;
        {
            let mut meta = tx.open_table(META_TABLE).map_err(db_err)?;
            meta.insert("salt", hex_encode(salt).as_bytes())
                .map_err(db_err)?;
            meta.insert("policy", policy.tag().as_bytes())
                .map_err(db_err)?;
            let canary = seal(key, KEYRING_DOMAIN)?;
            meta.insert(CANARY_KEY, canary.as_slice()).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }

    fn verify_existing(db: &Database, key: &[u8; 32], policy: &UnlockPolicy) -> Result<()> {
        let tx = db.begin_read().map_err(db_err)?;
        let table = tx.open_table(META_TABLE).map_err(db_err)?;
        let stored_tag: Option<String> = table
            .get("policy")
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8(v.value().to_vec()).ok());
        if stored_tag.as_deref() != Some(policy.tag().as_str()) {
            return Err(AIOSException::PermissionDenied(format!(
                "vault was created with unlock policy '{}', requested '{}'",
                stored_tag.unwrap_or_else(|| "<none>".into()),
                policy.tag()
            )));
        }
        let canary = table
            .get(CANARY_KEY)
            .ok()
            .flatten()
            .map(|v| v.value().to_vec());
        drop(table);
        match canary {
            Some(blob) => {
                let opened = unseal(key, &blob)?;
                if opened != KEYRING_DOMAIN {
                    return Err(AIOSException::PermissionDenied("canary mismatch".into()));
                }
                Ok(())
            }
            None => Err(AIOSException::ConfigurationError(
                "vault canary missing".into(),
            )),
        }
    }

    /// Store/overwrite a secret under `key`.
    pub fn set_secret(&self, key: &str, secret: &str) -> Result<()> {
        let blob = seal(&self.key, secret.as_bytes())?;
        let tx = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = tx.open_table(SECRETS_TABLE).map_err(db_err)?;
            table.insert(key, blob.as_slice()).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }

    /// Read a secret; `Ok(None)` when absent, error when undecryptable.
    pub fn get_secret(&self, key: &str) -> Result<Option<String>> {
        let tx = self.db.begin_read().map_err(db_err)?;
        let Ok(table) = tx.open_table(SECRETS_TABLE) else {
            return Ok(None);
        };
        match table.get(key).map_err(db_err)? {
            Some(row) => {
                let plain = unseal(&self.key, row.value())?;
                String::from_utf8(plain)
                    .map(Some)
                    .map_err(|_| AIOSException::InvalidPayload("secret is not valid utf-8".into()))
            }
            None => Ok(None),
        }
    }

    /// Remove a secret; returns false when it did not exist.
    pub fn delete_secret(&self, key: &str) -> Result<bool> {
        let existed = self.contains_key(key)?;
        let tx = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = tx.open_table(SECRETS_TABLE).map_err(db_err)?;
            table.remove(key).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok(existed)
    }

    /// Whether a secret exists under `key`.
    pub fn contains_key(&self, key: &str) -> Result<bool> {
        let tx = self.db.begin_read().map_err(db_err)?;
        let Ok(table) = tx.open_table(SECRETS_TABLE) else {
            return Ok(false);
        };
        Ok(table.get(key).map_err(db_err)?.is_some())
    }

    /// All secret keys, sorted lexicographically (values stay sealed).
    pub fn list_keys(&self) -> Result<Vec<String>> {
        let tx = self.db.begin_read().map_err(db_err)?;
        let Ok(table) = tx.open_table(SECRETS_TABLE) else {
            return Ok(Vec::new());
        };
        let mut keys = Vec::new();
        for row in table.iter().map_err(db_err)? {
            let (k, _) = row.map_err(db_err)?;
            keys.push(k.value().to_string());
        }
        keys.sort();
        Ok(keys)
    }

    /// Number of stored secrets.
    pub fn len(&self) -> Result<usize> {
        Ok(self.list_keys()?.len())
    }

    /// True when no secrets are stored.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Re-key every entry under a fresh master password.
    ///
    /// Only valid for password-policy vaults; generates a new salt so old
    /// backups become unreadable immediately. The handle stays usable and
    /// transparently switches to the rotated key.
    pub fn change_master_password(&mut self, new_password: &str) -> Result<()> {
        let tx = self.db.begin_read().map_err(db_err)?;
        let table = tx.open_table(SECRETS_TABLE).map_err(db_err)?;
        let mut entries = Vec::new();
        for row in table.iter().map_err(db_err)? {
            let (k, v) = row.map_err(db_err)?;
            let plain = unseal(&self.key, v.value())?;
            entries.push((k.value().to_string(), plain));
        }
        drop(table);
        drop(tx);

        let salt = random_bytes(16);
        let new_policy = UnlockPolicy::MasterPassword(new_password.to_string());
        let new_key = derive_key(&new_policy, &salt);

        let tx = self.db.begin_write().map_err(db_err)?;
        {
            {
                let mut meta = tx.open_table(META_TABLE).map_err(db_err)?;
                meta.insert("salt", hex_encode(&salt).as_bytes())
                    .map_err(db_err)?;
                meta.insert("policy", new_policy.tag().as_bytes())
                    .map_err(db_err)?;
                let canary = seal(&new_key, KEYRING_DOMAIN)?;
                meta.insert(CANARY_KEY, canary.as_slice()).map_err(db_err)?;
            }
            let mut table = tx.open_table(SECRETS_TABLE).map_err(db_err)?;
            for (k, plain) in entries {
                let blob = seal(&new_key, &plain)?;
                table.insert(k.as_str(), blob.as_slice()).map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)?;

        // Keep this handle usable after the re-key.
        self.key = new_key;
        Ok(())
    }
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    (0..s.len().div_ceil(2))
        .filter_map(|i| s.get(i * 2..i * 2 + 2))
        .map(|pair| u8::from_str_radix(pair, 16).ok())
        .collect::<Option<Vec<u8>>>()
        .filter(|v| !v.is_empty())
}

/// Accept any redb error flavor (`DatabaseError`, `TransactionError`,
/// `TableError`, `CommitError`, ...) — they all implement `Display`.
fn db_err<E: std::fmt::Display>(e: E) -> AIOSException {
    AIOSException::ConfigurationError(format!("keyring db: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("aios_keyring_test_{name}.redb"));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn roundtrip_store_and_fetch_unicode_secret() {
        let path = temp_path("roundtrip");
        let vault =
            KeyringVault::open(&path, &UnlockPolicy::MasterPassword("hunter2".into())).unwrap();
        vault.set_secret("llm/groq", "gsk_ключ-секрет🔑").unwrap();
        assert_eq!(
            vault.get_secret("llm/groq").unwrap().as_deref(),
            Some("gsk_ключ-секрет🔑")
        );
    }

    #[test]
    fn overwrite_replaces_value_in_place() {
        let path = temp_path("overwrite");
        let vault = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("p".into())).unwrap();
        vault.set_secret("wpa/home-net", "old-pass").unwrap();
        vault.set_secret("wpa/home-net", "new-pass").unwrap();
        assert_eq!(
            vault.get_secret("wpa/home-net").unwrap().as_deref(),
            Some("new-pass")
        );
        assert_eq!(vault.len().unwrap(), 1);
    }

    #[test]
    fn missing_key_is_none_not_error() {
        let path = temp_path("missing");
        let vault = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("p".into())).unwrap();
        assert_eq!(vault.get_secret("absent").unwrap(), None);
        assert!(vault.is_empty().unwrap());
    }

    #[test]
    fn wrong_master_password_fails_at_open() {
        let path = temp_path("wrongpass");
        KeyringVault::open(&path, &UnlockPolicy::MasterPassword("correct-horse".into())).unwrap();
        let err = KeyringVault::open(
            &path,
            &UnlockPolicy::MasterPassword("battery-staple".into()),
        )
        .unwrap_err();
        assert!(matches!(err, AIOSException::PermissionDenied(_)));
    }

    #[test]
    fn tee_bound_vault_reopens_with_same_platform_and_rejects_others() {
        let path = temp_path("tee");
        KeyringVault::open(&path, &UnlockPolicy::TeePlatform { platform_id: 777 }).unwrap();
        KeyringVault::open(&path, &UnlockPolicy::TeePlatform { platform_id: 777 }).unwrap();
        let other =
            KeyringVault::open(&path, &UnlockPolicy::TeePlatform { platform_id: 888 }).unwrap_err();
        assert!(matches!(other, AIOSException::PermissionDenied(_)));
    }

    #[test]
    fn policy_mismatch_between_password_and_tee_is_denied() {
        let path = temp_path("mixed");
        KeyringVault::open(&path, &UnlockPolicy::MasterPassword("p".into())).unwrap();
        let err =
            KeyringVault::open(&path, &UnlockPolicy::TeePlatform { platform_id: 1 }).unwrap_err();
        assert!(matches!(err, AIOSException::PermissionDenied(m) if m.contains("unlock policy")));
    }

    #[test]
    fn delete_reports_presence_accurately() {
        let path = temp_path("delete");
        let vault = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("p".into())).unwrap();
        vault.set_secret("a", "1").unwrap();
        assert!(vault.delete_secret("a").unwrap());
        assert!(!vault.delete_secret("a").unwrap());
        assert!(!vault.contains_key("a").unwrap());
    }

    #[test]
    fn list_keys_is_sorted_and_excludes_metadata() {
        let path = temp_path("list");
        let vault = KeyringVault::open(&path, &UnlockPolicy::MasterPassword("p".into())).unwrap();
        for k in ["z-token", "api/openrouter", "api/groq"] {
            vault.set_secret(k, "v").unwrap();
        }
        assert_eq!(
            vault.list_keys().unwrap(),
            vec!["api/groq", "api/openrouter", "z-token"]
        );
    }

    #[test]
    fn change_master_password_rotates_every_entry() {
        let path = temp_path("rekey");
        let mut vault =
            KeyringVault::open(&path, &UnlockPolicy::MasterPassword("old".into())).unwrap();
        vault.set_secret("k1", "value-one").unwrap();
        vault.set_secret("k2", "value-two").unwrap();

        vault.change_master_password("brand-new-pass").unwrap();
        drop(vault);

        let reopened = KeyringVault::open(
            &path,
            &UnlockPolicy::MasterPassword("brand-new-pass".into()),
        )
        .unwrap();
        assert_eq!(
            reopened.get_secret("k1").unwrap().as_deref(),
            Some("value-one")
        );
        assert_eq!(
            reopened.get_secret("k2").unwrap().as_deref(),
            Some("value-two")
        );
        drop(reopened);

        let old =
            KeyringVault::open(&path, &UnlockPolicy::MasterPassword("old".into())).unwrap_err();
        assert!(matches!(old, AIOSException::PermissionDenied(_)));
    }

    #[test]
    fn debug_impl_never_leaks_the_password() {
        let policy = UnlockPolicy::MasterPassword("super-secret".into());
        let rendered = format!("{policy:?}");
        assert!(!rendered.contains("super-secret"));
        assert!(rendered.contains("****"));
    }

    #[test]
    fn hex_helpers_roundtrip() {
        let raw = [0x00, 0x0f, 0xa5, 0xff];
        let enc = hex_encode(&raw);
        assert_eq!(enc, "000fa5ff");
        assert_eq!(hex_decode(&enc).unwrap(), raw.to_vec());
    }

    #[test]
    fn unseal_rejects_short_blobs_without_panicking() {
        let key = [7u8; 32];
        assert!(unseal(&key, b"short").is_err());
    }
}
