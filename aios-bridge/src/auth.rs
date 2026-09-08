//! Lightweight local authentication for the AIOS Bridge.
//!
//! Users are stored in a JSON file under the AIOS data directory. Passwords
//! are salted and key-stretched with SHA-256 (no external crypto dependency).
//! Sessions use a self-signed HMAC-SHA256 token that the client returns in the
//! `Authorization: Bearer <token>` header.

use crate::error::{BridgeError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Number of SHA-256 stretching iterations applied to a password+salt.
const STRETCH_ITERATIONS: u32 = 10_000;
/// Token lifetime in seconds (12 hours).
const TOKEN_TTL_SECS: u64 = 12 * 60 * 60;

/// A user record persisted to the auth store.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UserRecord {
    pub username: String,
    pub salt: String,
    pub password_hash: String,
    pub created_at: u64,
}

/// Payload embedded in a session token.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TokenPayload {
    pub sub: String,
    pub exp: u64,
    pub iat: u64,
}

/// In-memory copy of the auth store guarded by a mutex.
pub struct AuthStore {
    users: HashMap<String, UserRecord>,
    secret: String,
    path: PathBuf,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn stretch_hash(salt: &str, password: &str) -> String {
    let mut hash = format!("{salt}:{password}");
    for _ in 0..STRETCH_ITERATIONS {
        let mut h = Sha256::new();
        h.update(hash.as_bytes());
        let digest = h.finalize();
        hash = format!("{:x}", digest);
    }
    hash
}

fn b64_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

fn b64_decode(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| BridgeError::InvalidRequest("Malformed token".into()))
}

impl AuthStore {
    /// Create an auth store backed by `<data_dir>/users.json`.
    pub fn new(data_dir: &std::path::Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir).map_err(|e| {
            BridgeError::ServerError(format!("Cannot create data dir {data_dir:?}: {e}"))
        })?;
        let path = data_dir.join("users.json");
        let secret = std::env::var("AIOS_AUTH_SECRET").unwrap_or_else(|_| {
            // Derive a stable secret from a machine path when not configured.
            format!("aios-bridge-{}", data_dir.to_string_lossy())
        });
        let users = if path.exists() {
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                BridgeError::ServerError(format!("Cannot read auth store: {e}"))
            })?;
            serde_json::from_str::<HashMap<String, UserRecord>>(&raw).unwrap_or_default()
        } else {
            HashMap::new()
        };
        Ok(Self { users, secret, path })
    }

    /// Path used for the backing user store.
    pub fn users_path(&self) -> &std::path::Path {
        &self.path
    }

    fn persist(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.users).map_err(|e| {
            BridgeError::SerializationFailed(format!("Users serialization failed: {e}"))
        })?;
        std::fs::write(&self.path, json).map_err(|e| {
            BridgeError::ServerError(format!("Cannot write auth store: {e}"))
        })
    }

    fn sign_token(&self, payload: &TokenPayload) -> String {
        let header = b64_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let body = b64_encode(
            serde_json::to_vec(payload)
                .expect("token payload is always serializable")
                .as_slice(),
        );
        let signing_input = format!("{header}.{body}");
        let sig = self.hmac_sha256(signing_input.as_bytes());
        format!("{signing_input}.{}", b64_encode(&sig))
    }

    fn verify_token(&self, token: &str) -> Result<TokenPayload> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(BridgeError::InvalidRequest("Malformed token".into()));
        }
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let expected = self.hmac_sha256(signing_input.as_bytes());
        let given = b64_decode(parts[2])?;
        if expected.len() != given.len()
            || expected
                .iter()
                .zip(given.iter())
                .any(|(a, b)| a != b)
        {
            return Err(BridgeError::InvalidRequest("Invalid token signature".into()));
        }
        let payload: TokenPayload = serde_json::from_slice(&b64_decode(parts[1])?).map_err(|_| {
            BridgeError::InvalidRequest("Invalid token payload".into())
        })?;
        if payload.exp <= now_secs() {
            return Err(BridgeError::InvalidRequest("Token expired".into()));
        }
        Ok(payload)
    }

    /// Standard HMAC-SHA256 (RFC 2104) built on the `sha2` crate.
    fn hmac_sha256(&self, data: &[u8]) -> Vec<u8> {
        const BLOCK: usize = 64;
        let mut key = self.secret.as_bytes().to_vec();
        if key.len() > BLOCK {
            let mut h = Sha256::new();
            h.update(&key);
            key = h.finalize().to_vec();
        }
        let mut key_padded = [0u8; BLOCK];
        key_padded[..key.len()].copy_from_slice(&key);

        let mut ipad = vec![0u8; BLOCK];
        let mut opad = vec![0u8; BLOCK];
        for i in 0..BLOCK {
            ipad[i] = key_padded[i] ^ 0x36;
            opad[i] = key_padded[i] ^ 0x5c;
        }

        let mut inner = Sha256::new();
        inner.update(&ipad);
        inner.update(data);
        let inner_digest = inner.finalize();

        let mut outer = Sha256::new();
        outer.update(&opad);
        outer.update(inner_digest);
        outer.finalize().to_vec()
    }

    /// Register a new user. Returns a session token on success.
    pub fn register(&mut self, username: &str, password: &str) -> Result<String> {
        let username = username.trim().to_lowercase();
        if username.is_empty() || password.is_empty() {
            return Err(BridgeError::InvalidRequest(
                "Username and password are required".into(),
            ));
        }
        if username.len() < 3 {
            return Err(BridgeError::InvalidRequest(
                "Username must be at least 3 characters".into(),
            ));
        }
        if password.len() < 6 {
            return Err(BridgeError::InvalidRequest(
                "Password must be at least 6 characters".into(),
            ));
        }
        if self.users.contains_key(&username) {
            return Err(BridgeError::InvalidRequest(
                "Username already exists".into(),
            ));
        }
        let salt = uuid::Uuid::new_v4().to_string();
        let password_hash = stretch_hash(&salt, password);
        let record = UserRecord {
            username: username.clone(),
            salt,
            password_hash,
            created_at: now_secs(),
        };
        self.users.insert(username.clone(), record);
        self.persist()?;
        self.issue_token(&username)
    }

    /// Authenticate a user by username/password. Returns a session token.
    pub fn login(&mut self, username: &str, password: &str) -> Result<String> {
        let username = username.trim().to_lowercase();
        let Some(record) = self.users.get(&username) else {
            return Err(BridgeError::InvalidRequest("Invalid credentials".into()));
        };
        let candidate = stretch_hash(&record.salt, password);
        if candidate != record.password_hash {
            return Err(BridgeError::InvalidRequest("Invalid credentials".into()));
        }
        self.issue_token(&username)
    }

    /// The username of a valid token.
    pub fn bearer_user(&self, header: &str) -> Result<String> {
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| BridgeError::InvalidRequest("Bearer token required".into()))?
            .trim();
        let payload = self.verify_token(token)?;
        if self.users.contains_key(&payload.sub) {
            Ok(payload.sub)
        } else {
            Err(BridgeError::InvalidRequest("Unknown user".into()))
        }
    }

    /// Whether a user exists (used for the `/me` endpoint).
    pub fn user_exists(&self, username: &str) -> bool {
        self.users.contains_key(&username.to_lowercase())
    }

    /// Number of registered users.
    pub fn user_count(&self) -> usize {
        self.users.len()
    }

    fn issue_token(&self, username: &str) -> Result<String> {
        let now = now_secs();
        let payload = TokenPayload {
            sub: username.to_string(),
            iat: now,
            exp: now + TOKEN_TTL_SECS,
        };
        Ok(self.sign_token(&payload))
    }
}

/// Convenience alias for `Arc<Mutex<AuthStore>>` held in bridge state.
pub type AuthHandle = std::sync::Arc<Mutex<AuthStore>>;

/// Build an `AuthHandle` from the AIOS data dir.
pub fn open_auth(data_dir: &std::path::Path) -> Result<AuthHandle> {
    Ok(std::sync::Arc::new(Mutex::new(AuthStore::new(data_dir)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, AuthStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = AuthStore::new(dir.path()).expect("auth store");
        (dir, store)
    }

    #[test]
    fn register_and_login_roundtrip() {
        let (_dir, mut store) = temp_store();
        let token = store.register("alice", "secret123").expect("register");
        assert!(!token.is_empty());
        // Login with wrong password fails.
        assert!(store.login("alice", "wrongpass").is_err());
        // Login with correct password issues a token.
        let token2 = store.login("alice", "secret123").expect("login");
        assert!(!token2.is_empty());
    }

    #[test]
    fn duplicate_register_rejected() {
        let (_dir, mut store) = temp_store();
        store.register("bob", "secret123").expect("register");
        assert!(store.register("bob", "secret456").is_err());
    }

    #[test]
    fn bearer_user_validation() {
        let (_dir, mut store) = temp_store();
        let token = store.register("carol", "secret123").expect("register");
        let user = store
            .bearer_user(&format!("Bearer {token}"))
            .expect("valid token");
        assert_eq!(user, "carol");
    }

    #[test]
    fn bad_token_rejected() {
        let (_dir, store) = temp_store();
        assert!(store.bearer_user("Bearer nonsense.token.here").is_err());
        assert!(store.bearer_user("garbage").is_err());
    }

    #[test]
    fn passwords_not_stored_in_plaintext() {
        let (_dir, mut store) = temp_store();
        store.register("dave", "secret123").expect("register");
        let users: Vec<&UserRecord> = store.users.values().collect();
        assert!(!users[0].password_hash.contains("secret123"));
        assert_ne!(users[0].salt, users[0].password_hash);
    }

    #[test]
    fn same_password_different_salt_differs() {
        let (_dir, mut store) = temp_store();
        store.register("eve", "samepass").expect("register");
        store.register("frank", "samepass").expect("register");
        let a = store.users.get("eve").unwrap();
        let b = store.users.get("frank").unwrap();
        assert_ne!(a.password_hash, b.password_hash);
        assert_ne!(a.salt, b.salt);
    }

    #[test]
    fn validation_checks() {
        let (_dir, mut store) = temp_store();
        assert!(store.register("ab", "secret123").is_err()); // too short
        assert!(store.register("valid", "123").is_err()); // password too short
        assert!(store.register("", "secret123").is_err()); // empty
    }
}
