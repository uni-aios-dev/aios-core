use serde::{Deserialize, Serialize};

/// The kind of a block store source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    /// A GitHub repository containing `index.json` and `blocks/*.wasm`.
    GitHub,
    /// A local directory with `index.json` or `*.wasm`/`*.bin` files.
    Local,
    /// An HTTP update service exposing `/index.json` and `/blocks/*.wasm`.
    Http,
}

/// Hex-encoded Ed25519 public key trusted to sign official AIOS store
/// manifests, configured via `AIOS_OFFICIAL_PUBLIC_KEY`.
pub fn official_public_key() -> Option<String> {
    std::env::var("AIOS_OFFICIAL_PUBLIC_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A named source of block catalogs and binaries.
///
/// `base` is interpreted depending on `kind`:
/// - GitHub: `owner/repo`
/// - Local: absolute or relative directory path
/// - Http: root URL of the update service (without trailing slash)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreSource {
    /// Unique source name (derived from kind + base by default).
    pub name: String,
    /// Source type.
    pub kind: SourceKind,
    /// Source-specific base location.
    pub base: String,
    /// Hex-encoded Ed25519 public keys that manifests from this source must be
    /// signed by. Empty means no signature is required from this source.
    #[serde(default)]
    pub trusted_public_keys: Vec<String>,
}

impl StoreSource {
    /// Official community store hosted on GitHub. When `AIOS_OFFICIAL_PUBLIC_KEY`
    /// is set, manifests from it must be signed by that key.
    pub fn github_default() -> Self {
        let mut source = Self::github("uni-aios-dev/aios-official-store");
        if let Some(key) = official_public_key() {
            source.trusted_public_keys.push(key);
        }
        source
    }

    /// GitHub source for `owner/repo`.
    pub fn github(owner_repo: &str) -> Self {
        let base = owner_repo
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_start_matches("http://github.com/")
            .trim_end_matches('/')
            .to_string();
        Self {
            name: format!("github:{base}"),
            kind: SourceKind::GitHub,
            base,
            trusted_public_keys: Vec::new(),
        }
    }

    /// GitHub source that additionally requires manifests signed by `keys`.
    pub fn github_with_keys(owner_repo: &str, keys: Vec<String>) -> Self {
        let mut source = Self::github(owner_repo);
        source.trusted_public_keys = keys;
        source
    }

    /// Local directory source.
    pub fn local(path: &str) -> Self {
        Self {
            name: format!("local:{path}"),
            kind: SourceKind::Local,
            base: path.to_string(),
            trusted_public_keys: Vec::new(),
        }
    }

    /// HTTP update-service source.
    pub fn http(url: &str) -> Self {
        Self {
            name: format!("http:{}", url.trim_end_matches('/')),
            kind: SourceKind::Http,
            base: url.trim_end_matches('/').to_string(),
            trusted_public_keys: Vec::new(),
        }
    }

    /// URL of the block catalog for remote sources; `None` for local dirs.
    pub fn index_url(&self) -> Option<String> {
        match self.kind {
            SourceKind::GitHub => Some(format!(
                "https://raw.githubusercontent.com/{}/HEAD/index.json",
                self.base
            )),
            SourceKind::Http => Some(format!("{}/index.json", self.base)),
            SourceKind::Local => None,
        }
    }

    /// URL to download `name` for remote sources; `None` for local dirs.
    pub fn block_url(&self, name: &str) -> Option<String> {
        match self.kind {
            SourceKind::GitHub => Some(format!(
                "https://raw.githubusercontent.com/{}/HEAD/blocks/{}.wasm",
                self.base, name
            )),
            SourceKind::Http => Some(format!("{}/blocks/{}.wasm", self.base, name)),
            SourceKind::Local => None,
        }
    }

    /// Human-readable source description.
    pub fn display(&self) -> String {
        match self.kind {
            SourceKind::GitHub => format!("github [{}]", self.base),
            SourceKind::Local => format!("local [{}]", self.base),
            SourceKind::Http => format!("http [{}]", self.base),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_github_default_name() {
        let s = StoreSource::github_default();
        assert_eq!(s.kind, SourceKind::GitHub);
        assert_eq!(s.base, "uni-aios-dev/aios-official-store");
    }

    #[test]
    fn test_github_strips_url_prefix() {
        let s = StoreSource::github("https://github.com/acme/blocks/");
        assert_eq!(s.base, "acme/blocks");
    }

    #[test]
    fn test_github_index_url() {
        let s = StoreSource::github("acme/blocks");
        assert_eq!(
            s.index_url().unwrap(),
            "https://raw.githubusercontent.com/acme/blocks/HEAD/index.json"
        );
    }

    #[test]
    fn test_github_block_url() {
        let s = StoreSource::github("acme/blocks");
        assert_eq!(
            s.block_url("net").unwrap(),
            "https://raw.githubusercontent.com/acme/blocks/HEAD/blocks/net.wasm"
        );
    }

    #[test]
    fn test_http_urls() {
        let s = StoreSource::http("http://127.0.0.1:8080/");
        assert_eq!(s.index_url().unwrap(), "http://127.0.0.1:8080/index.json");
        assert_eq!(
            s.block_url("x").unwrap(),
            "http://127.0.0.1:8080/blocks/x.wasm"
        );
    }

    #[test]
    fn test_local_has_no_urls() {
        let s = StoreSource::local("/tmp/blocks");
        assert!(s.index_url().is_none());
        assert!(s.block_url("x").is_none());
    }

    #[test]
    fn test_display_mentions_kind() {
        assert!(StoreSource::github_default().display().contains("github"));
        assert!(StoreSource::local("/tmp").display().contains("local"));
    }
}
