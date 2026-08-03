//! Catalog fetching and block downloading from [`StoreSource`]s.
use crate::manifest::ManifestInfo;
use crate::source::{SourceKind, StoreSource};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Fetch the block catalog of `source`.
///
/// Local sources are scanned synchronously; remote sources are fetched over
/// HTTP with `client`.
pub async fn fetch_index(
    source: &StoreSource,
    client: &reqwest::Client,
) -> Result<Vec<ManifestInfo>, String> {
    if source.kind == SourceKind::Local {
        return fetch_index_local(source);
    }
    let url = source
        .index_url()
        .ok_or_else(|| format!("Source '{}' has no catalog URL", source.display()))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP error while fetching {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Catalog request to {url} failed with status {}",
            response.status()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Read error from {url}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Catalog JSON error from {url}: {e}"))
}

/// Scan a local directory for a block catalog.
///
/// Prefers a hand-written `index.json`; otherwise walks `*.wasm`/`*.bin` files
/// and builds manifests from the file name and an optional `<name>_<version>.json`
/// sidecar.
pub fn fetch_index_local(source: &StoreSource) -> Result<Vec<ManifestInfo>, String> {
    let dir = Path::new(&source.base);
    if !dir.is_dir() {
        return Err(format!("Local source directory not found: {:?}", dir));
    }
    let index_path = dir.join("index.json");
    if index_path.is_file() {
        let data = std::fs::read_to_string(&index_path)
            .map_err(|e| format!("Failed to read {:?}: {e}", index_path))?;
        let mut catalog: Vec<ManifestInfo> =
            serde_json::from_str(&data).map_err(|e| format!("Catalog JSON error: {e}"))?;
        for m in catalog.iter_mut() {
            if m.store_url.is_none() {
                m.store_url = Some(source.base.clone());
            }
        }
        return Ok(catalog);
    }
    scan_local_blocks(dir)
}

fn scan_local_blocks(dir: &Path) -> Result<Vec<ManifestInfo>, String> {
    let mut catalog = Vec::new();
    let mut files = collect_block_files(dir);
    files.sort();

    for path in files {
        let (name, version) = parse_name_version(&path);
        let binary = std::fs::read(&path).map_err(|e| format!("Failed to read {:?}: {e}", path))?;
        let hash = hex::encode(Sha256::digest(&binary));

        let mut manifest = ManifestInfo {
            name: name.clone(),
            version,
            description: String::new(),
            author: "local".into(),
            capabilities: HashSet::new(),
            wasm_size_bytes: binary.len() as u64,
            wasm_sha256: hash,
            signature: None,
            store_url: None,
        };

        let sidecar = path.with_extension("json");
        if sidecar.is_file() {
            if let Ok(data) = std::fs::read_to_string(&sidecar) {
                if let Ok(m) = serde_json::from_str::<ManifestInfo>(&data) {
                    manifest.name = m.name;
                    manifest.version = m.version;
                    manifest.description = m.description;
                    manifest.author = m.author;
                    manifest.capabilities = m.capabilities;
                }
            }
        }
        catalog.push(manifest);
    }
    Ok(catalog)
}

/// Collect `*.wasm`/`*.bin` files from `dir` and its `blocks/` subdirectory.
fn collect_block_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for base in [dir.to_path_buf(), dir.join("blocks")] {
        if !base.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let ext = path.extension().and_then(|e| e.to_str());
                if ext == Some("wasm") || ext == Some("bin") {
                    files.push(path);
                }
            }
        }
    }
    files
}

/// Derive `(name, version)` from a file named `<name>_<version>.wasm`.
pub fn parse_name_version(path: &Path) -> (String, String) {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("block")
        .to_string();
    match stem.split_once('_') {
        Some((name, version)) if !version.is_empty() => (name.to_string(), version.to_string()),
        _ => (stem, "0.0.0".to_string()),
    }
}

/// Download the WASM binary of a block from a remote source.
pub async fn download_block(
    source: &StoreSource,
    name: &str,
    client: &reqwest::Client,
) -> Result<Vec<u8>, String> {
    if source.kind == SourceKind::Local {
        return download_block_local(source, name);
    }
    let url = source
        .block_url(name)
        .ok_or_else(|| format!("Source '{}' cannot serve binaries", source.display()))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP error while downloading {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Download of {url} failed with status {}",
            response.status()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Read error from {url}: {e}"))?;
    Ok(bytes.to_vec())
}

/// Read a block binary from a local directory, matching `<name>_*.wasm`/`.bin`.
pub fn download_block_local(source: &StoreSource, name: &str) -> Result<Vec<u8>, String> {
    let dir = Path::new(&source.base);
    let mut candidates: Vec<PathBuf> = collect_block_files(dir)
        .into_iter()
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.split('_').next() == Some(name))
                .unwrap_or(false)
        })
        .collect();
    candidates.sort();
    let path = candidates
        .pop()
        .ok_or_else(|| format!("Block '{name}' not found in local source {:?}", dir))?;
    std::fs::read(&path).map_err(|e| format!("Failed to read {:?}: {e}", path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_block(dir: &Path, name: &str, version: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(format!("{name}_{version}.wasm"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn test_parse_name_version() {
        let (n, v) = parse_name_version(Path::new("blocks/net_1.2.0.wasm"));
        assert_eq!(n, "net");
        assert_eq!(v, "1.2.0");
        let (n, v) = parse_name_version(Path::new("blocks/plain.bin"));
        assert_eq!(n, "plain");
        assert_eq!(v, "0.0.0");
    }

    #[test]
    fn test_fetch_index_local_index_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("index.json"),
            serde_json::json!([
                { "name": "alpha", "version": "1.0.0", "description": "a", "author": "t",
                  "capabilities": [], "wasm_size_bytes": 3, "wasm_sha256": "abc", "store_url": null }
            ])
            .to_string(),
        )
        .unwrap();
        let source = StoreSource::local(dir.path().to_str().unwrap());
        let catalog = fetch_index_local(&source).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "alpha");
        assert_eq!(
            catalog[0].store_url.as_deref(),
            Some(dir.path().to_str().unwrap())
        );
    }

    #[test]
    fn test_fetch_index_local_scans_wasm() {
        let dir = tempfile::tempdir().unwrap();
        write_block(dir.path(), "beta", "2.0.0", b"wasm-bytes");
        let source = StoreSource::local(dir.path().to_str().unwrap());
        let catalog = fetch_index_local(&source).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "beta");
        assert_eq!(catalog[0].version, "2.0.0");
        assert_eq!(
            catalog[0].wasm_sha256,
            hex::encode(Sha256::digest(b"wasm-bytes"))
        );
    }

    #[test]
    fn test_fetch_index_local_sidecar_enriches() {
        let dir = tempfile::tempdir().unwrap();
        write_block(dir.path(), "gamma", "1.0.0", b"x");
        std::fs::write(
            dir.path().join("gamma_1.0.0.json"),
            serde_json::json!({
                "name": "gamma", "version": "1.0.0", "description": "real name",
                "author": "jane", "capabilities": ["CAP_NET_CONNECT"],
                "wasm_sha256": "hash", "wasm_size_bytes": 1
            })
            .to_string(),
        )
        .unwrap();
        let source = StoreSource::local(dir.path().to_str().unwrap());
        let catalog = fetch_index_local(&source).unwrap();
        assert_eq!(catalog[0].author, "jane");
        assert_eq!(catalog[0].description, "real name");
        assert!(catalog[0].capabilities.contains("CAP_NET_CONNECT"));
    }

    #[test]
    fn test_fetch_index_local_missing_dir() {
        let source = StoreSource::local("/nonexistent/path/xyz");
        assert!(fetch_index_local(&source).is_err());
    }

    #[test]
    fn test_download_block_local() {
        let dir = tempfile::tempdir().unwrap();
        write_block(dir.path(), "delta", "1.0.0", b"first");
        write_block(dir.path(), "delta", "2.0.0", b"second");
        let source = StoreSource::local(dir.path().to_str().unwrap());
        let bytes = download_block_local(&source, "delta").unwrap();
        assert_eq!(bytes, b"second");
    }

    #[test]
    fn test_download_block_local_missing() {
        let dir = tempfile::tempdir().unwrap();
        let source = StoreSource::local(dir.path().to_str().unwrap());
        assert!(download_block_local(&source, "nope").is_err());
    }
}
