//! Secret resolution shared by AIOS components.
//!
//! Secrets are never compiled into binaries. They are read from
//! `AIOS_<NAME>_SECRET`, from `<AIOS_DATA_DIR>/secrets/<name>.key`, or generated
//! with a CSPRNG and persisted locally on first use.

use crate::error::{AIOSException, Result};
use getrandom::getrandom;
use std::path::{Path, PathBuf};

/// Minimum accepted secret length in bytes.
pub const MIN_SECRET_LEN: usize = 16;

/// Generate 32 cryptographically random bytes.
pub fn random_secret() -> Vec<u8> {
    let mut bytes = [0u8; 32];
    getrandom(&mut bytes).expect("AIOS: system CSPRNG unavailable");
    bytes.to_vec()
}

/// Default AIOS data directory (`AIOS_DATA_DIR` or `./aios_data`).
pub fn data_dir() -> PathBuf {
    std::env::var("AIOS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("aios_data"))
}

/// Environment variable name for a named secret.
pub fn env_name(name: &str) -> String {
    format!(
        "AIOS_{}_SECRET",
        name.trim().to_ascii_uppercase().replace(['-', '.'], "_")
    )
}

/// Resolve a named secret from the environment or `<data_dir>/secrets/`.
///
/// Missing secrets are generated and persisted with owner-only permissions on
/// Unix; an existing file is reused so issued tokens survive restarts.
pub fn resolve_secret_in(name: &str, data_dir: &Path) -> Result<Vec<u8>> {
    if let Ok(value) = std::env::var(env_name(name)) {
        return validate_secret(value.as_bytes(), env_name(name).as_str());
    }
    let path = secret_path(data_dir, name)?;
    if path.exists() {
        let bytes = std::fs::read(&path).map_err(|e| {
            AIOSException::ConfigurationError(format!("Cannot read secret {}: {e}", path.display()))
        })?;
        return validate_secret(&bytes, name);
    }
    let secret = random_secret();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AIOSException::ConfigurationError(format!("Cannot create secret dir: {e}"))
        })?;
    }
    write_secret(&path, &secret)?;
    Ok(secret)
}

/// Resolve a named secret under the default AIOS data directory.
pub fn resolve_secret(name: &str) -> Result<Vec<u8>> {
    resolve_secret_in(name, &data_dir())
}

/// Resolve a named secret, falling back to a per-process random value.
///
/// Used by in-process issuer keys where configuration errors must not create a
/// forgeable hardcoded secret. Tokens signed by the fallback are intentionally
/// invalid after a restart.
pub fn secret_or_ephemeral(name: &str) -> Vec<u8> {
    match resolve_secret(name) {
        Ok(secret) => secret,
        Err(e) => {
            log::warn!("SECRET: cannot resolve {name}, using ephemeral key: {e}");
            random_secret()
        }
    }
}

fn validate_secret(bytes: &[u8], source: &str) -> Result<Vec<u8>> {
    if bytes.len() < MIN_SECRET_LEN {
        return Err(AIOSException::ConfigurationError(format!(
            "Secret from {source} is shorter than {MIN_SECRET_LEN} bytes"
        )));
    }
    Ok(bytes.to_vec())
}

fn secret_path(data_dir: &Path, name: &str) -> Result<PathBuf> {
    let file = format!(
        "{}.key",
        name.trim()
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
    );
    if file == ".key" {
        return Err(AIOSException::ConfigurationError(
            "Invalid secret name".into(),
        ));
    }
    Ok(data_dir.join("secrets").join(file))
}

fn write_secret(path: &Path, secret: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| {
        AIOSException::ConfigurationError(format!(
            "Cannot create secret file {}: {e}",
            path.display()
        ))
    })?;
    use std::io::Write;
    file.write_all(secret).map_err(|e| {
        AIOSException::ConfigurationError(format!(
            "Cannot write secret file {}: {e}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_are_random_and_valid() {
        let a = random_secret();
        let b = random_secret();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(validate_secret(&a, "test").is_ok());
        assert!(validate_secret(b"short", "test").is_err());
    }

    #[test]
    fn secret_is_persisted_and_reused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = resolve_secret_in("unit_test", dir.path()).expect("first secret");
        let second = resolve_secret_in("unit_test", dir.path()).expect("second secret");
        assert_eq!(first, second);
        let path = dir.path().join("secrets").join("unit_test.key");
        assert!(path.exists());
    }

    #[test]
    fn invalid_name_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(resolve_secret_in("", dir.path()).is_err());
    }
}
