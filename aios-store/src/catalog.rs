//! Catalog fetching and block downloading from [`StoreSource`]s.
use crate::manifest::ManifestInfo;
use crate::source::{SourceKind, StoreSource};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Normalize a store capability token to the `CAP_*` form used by AIOS.
///
/// Both native tokens (`CAP_FS_READ`) and friendly aliases from the community
/// store (`filesystem:read`, `net:connect`, …) are accepted. Unknown tokens are
/// preserved so `ManifestValidator` can reject them explicitly.
fn normalize_capability(token: &str) -> String {
    let trimmed = token.trim();
    let upper = trimmed
        .to_ascii_uppercase()
        .replace([':', '-', ' '], "_")
        .replace("__", "_");
    match upper.as_str() {
        "CAP_NET_BIND" | "NETWORK_BIND" | "NET_BIND" => "CAP_NET_BIND".into(),
        "CAP_NET_CONNECT" | "NETWORK_CONNECT" | "NET_CONNECT" => "CAP_NET_CONNECT".into(),
        "CAP_NET_LISTEN" | "NETWORK_LISTEN" | "NET_LISTEN" => "CAP_NET_LISTEN".into(),
        "CAP_FS_READ" | "FILESYSTEM_READ" | "FS_READ" => "CAP_FS_READ".into(),
        "CAP_FS_WRITE" | "FILESYSTEM_WRITE" | "FS_WRITE" => "CAP_FS_WRITE".into(),
        "CAP_FS_DELETE" | "FILESYSTEM_DELETE" | "FS_DELETE" => "CAP_FS_DELETE".into(),
        "CAP_HW_ACCESS" | "HARDWARE_ACCESS" | "HW_ACCESS" => "CAP_HW_ACCESS".into(),
        "CAP_MEM_ALLOC" | "MEMORY_ALLOC" | "MEM_ALLOC" => "CAP_MEM_ALLOC".into(),
        "CAP_MEM_SHARE" | "MEMORY_SHARE" | "MEM_SHARE" => "CAP_MEM_SHARE".into(),
        "CAP_SCHED_MODIFY" | "SCHEDULER_MODIFY" | "SCHED_MODIFY" => "CAP_SCHED_MODIFY".into(),
        "CAP_BROWSER_RENDER" | "BROWSER_RENDER" => "CAP_BROWSER_RENDER".into(),
        other if other.starts_with("CAP_") => other.to_string(),
        _ => trimmed.to_string(),
    }
}

/// Read a manifest description from either a string or an `{ "en": …, "ru": … }`
/// object. English is preferred so the same catalog works in every locale.
fn description_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(object)) => object
            .get("en")
            .or_else(|| object.get("ru"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Convert one JSON manifest value into the host's [`ManifestInfo`].
///
/// Accepts both the original host fields (`wasm_sha256`, `description: String`,
/// `CAP_*`) and the community-store fields (`wasm_hash` with a `sha256:` prefix,
/// localized description objects and friendly capability aliases).
pub fn manifest_from_value(value: &Value) -> Result<ManifestInfo, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Manifest entry must be a JSON object".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "Manifest entry is missing 'name'".to_string())?;
    let version = object
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("0.0.0");
    let author = object
        .get("author")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let capabilities = object
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(normalize_capability)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let raw_hash = object
        .get("wasm_sha256")
        .or_else(|| object.get("wasm_hash"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let wasm_sha256 = raw_hash
        .strip_prefix("sha256:")
        .unwrap_or(raw_hash)
        .trim()
        .to_ascii_lowercase();
    let wasm_size_bytes = object
        .get("wasm_size_bytes")
        .or_else(|| object.get("wasm_size"))
        .or_else(|| object.get("size_bytes"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let signature = match object.get("signature") {
        Some(value) if !value.is_null() => Some(
            serde_json::from_value(value.clone()).map_err(|e| format!("Invalid signature: {e}"))?,
        ),
        _ => None,
    };
    let store_url = object
        .get("store_url")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(ManifestInfo {
        name: name.to_string(),
        version: version.to_string(),
        description: description_text(object.get("description")),
        author: author.to_string(),
        capabilities,
        wasm_size_bytes,
        wasm_sha256,
        signature,
        store_url,
    })
}

/// Parse a store catalog in either supported shape: a plain array of manifests
/// or the object form `{ "blocks": [ … ], "schema_version": … }`.
pub fn parse_index(text: &str) -> Result<Vec<ManifestInfo>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("Catalog JSON error: {e}"))?;
    let entries = match value {
        Value::Array(entries) => entries,
        Value::Object(mut object) => object
            .remove("blocks")
            .and_then(|value| value.as_array().cloned())
            .ok_or_else(|| "Catalog object is missing a 'blocks' array".to_string())?,
        other => {
            return Err(format!(
                "Catalog must be an array or an object with 'blocks', got {other}"
            ))
        }
    };
    entries.iter().map(manifest_from_value).collect()
}

fn enrich_from_binary(manifest: &mut ManifestInfo, binary: &[u8]) {
    manifest.wasm_size_bytes = binary.len() as u64;
    manifest.wasm_sha256 = hex::encode(Sha256::digest(binary));
}

/// Fetch the block catalog of `source`.
///
/// Local sources are scanned synchronously; remote sources are fetched over
/// HTTP with `client`. Missing hash/size fields are filled from the binary so
/// `store install` can still verify the download.
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
    let text = std::str::from_utf8(&bytes).map_err(|e| format!("Catalog is not UTF-8: {e}"))?;
    let mut catalog = parse_index(text)?;
    for manifest in catalog.iter_mut() {
        if manifest.wasm_sha256.is_empty() || manifest.wasm_size_bytes == 0 {
            if let Ok(binary) = download_block(source, &manifest.name, client).await {
                enrich_from_binary(manifest, &binary);
            }
        }
    }
    Ok(catalog)
}

/// Scan a local directory for a block catalog.
///
/// Prefers an `index.json` in either supported shape; otherwise walks
/// `*.wasm`/`*.bin` files (including `blocks/<name>/block.wasm`) and builds
/// manifests from the file name plus an optional sidecar `manifest.json`.
pub fn fetch_index_local(source: &StoreSource) -> Result<Vec<ManifestInfo>, String> {
    let dir = Path::new(&source.base);
    if !dir.is_dir() {
        return Err(format!("Local source directory not found: {:?}", dir));
    }
    let index_path = dir.join("index.json");
    if index_path.is_file() {
        let data = std::fs::read_to_string(&index_path)
            .map_err(|e| format!("Failed to read {:?}: {e}", index_path))?;
        let mut catalog = parse_index(&data)?;
        for manifest in catalog.iter_mut() {
            if manifest.store_url.is_none() {
                manifest.store_url = Some(source.base.clone());
            }
            if manifest.wasm_sha256.is_empty() || manifest.wasm_size_bytes == 0 {
                if let Ok(binary) = download_block_local(source, &manifest.name) {
                    enrich_from_binary(manifest, &binary);
                }
            }
        }
        return Ok(catalog);
    }
    scan_local_blocks(dir)
}

fn scan_local_blocks(dir: &Path) -> Result<Vec<ManifestInfo>, String> {
    let mut catalog = Vec::new();
    let files = collect_block_files(dir);

    for path in files {
        let (fallback_name, fallback_version) = parse_name_version(&path);
        let binary = std::fs::read(&path).map_err(|e| format!("Failed to read {:?}: {e}", path))?;
        let mut manifest = ManifestInfo {
            name: fallback_name.clone(),
            version: fallback_version,
            description: String::new(),
            author: "local".into(),
            capabilities: HashSet::new(),
            wasm_size_bytes: binary.len() as u64,
            wasm_sha256: hex::encode(Sha256::digest(&binary)),
            signature: None,
            store_url: None,
        };

        let nested = is_nested_block_file(&path);
        let sidecar = if nested {
            path.parent()
                .map(|parent| parent.join("manifest.json"))
                .unwrap_or_default()
        } else {
            path.with_extension("json")
        };
        if sidecar.is_file() {
            if let Ok(data) = std::fs::read_to_string(&sidecar) {
                if let Ok(value) = serde_json::from_str::<Value>(&data) {
                    if let Ok(m) = manifest_from_value(&value) {
                        manifest.name = m.name;
                        manifest.version = m.version;
                        manifest.description = m.description;
                        manifest.author = m.author;
                        manifest.capabilities = m.capabilities;
                    }
                }
            }
        }
        if manifest.name == "block" {
            manifest.name = fallback_name;
        }
        catalog.push(manifest);
    }
    Ok(catalog)
}

fn is_block_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("wasm") | Some("bin")
    )
}

fn is_nested_block_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("block.wasm") | Some("block.bin")
    )
}

/// Collect `*.wasm`/`*.bin` files from `dir`, its `blocks/` subdirectory and
/// nested `blocks/<name>/` directories.
fn collect_block_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for base in [dir.to_path_buf(), dir.join("blocks")] {
        if !base.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if is_block_file(&path) {
                        files.push(path);
                    }
                } else if path.is_dir() {
                    if let Ok(inner) = std::fs::read_dir(&path) {
                        for child in inner.flatten().map(|entry| entry.path()) {
                            if child.is_file() && is_block_file(&child) {
                                files.push(child);
                            }
                        }
                    }
                }
            }
        }
    }
    files.sort();
    files.dedup();
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
    if response.status().is_success() {
        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Read error from {url}: {e}"))?;
        return Ok(bytes.to_vec());
    }
    if let Some(fallback) = source.legacy_block_url(name) {
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            let response = client
                .get(&fallback)
                .send()
                .await
                .map_err(|e| format!("HTTP error while downloading {fallback}: {e}"))?;
            if response.status().is_success() {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| format!("Read error from {fallback}: {e}"))?;
                return Ok(bytes.to_vec());
            }
        }
    }
    Err(format!(
        "Download of {url} failed with status {}",
        response.status()
    ))
}

/// Read a block binary from a local source, preferring the nested
/// `blocks/<name>/block.wasm` layout over legacy `<name>_<version>.wasm` files.
pub fn download_block_local(source: &StoreSource, name: &str) -> Result<Vec<u8>, String> {
    let dir = Path::new(&source.base);
    let nested = dir.join("blocks").join(name).join("block.wasm");
    if nested.is_file() {
        return std::fs::read(&nested).map_err(|e| format!("Failed to read {:?}: {e}", nested));
    }
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
    fn test_fetch_index_local_deduplicates_blocks_directory() {
        let dir = tempfile::tempdir().unwrap();
        let blocks = dir.path().join("blocks");
        std::fs::create_dir_all(&blocks).unwrap();
        write_block(&blocks, "net", "1.0.0", b"binary");
        let source = StoreSource::local(dir.path().to_str().unwrap());
        let catalog = fetch_index_local(&source).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "net");
        assert_eq!(catalog[0].version, "1.0.0");
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

    #[test]
    fn test_parse_object_index_with_localized_description() {
        let text = serde_json::json!({
            "schema_version": "1.0",
            "name": "aios-official-store",
            "blocks": [{
                "name": "demo",
                "version": "1.2.3",
                "description": { "en": "Demo block", "ru": "Демо-блок" },
                "author": "team",
                "capabilities": ["filesystem:read", "CAP_NET_CONNECT"],
                "wasm_hash": "sha256:ABCD",
                "wasm_size_bytes": 12
            }]
        })
        .to_string();
        let catalog = parse_index(&text).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].description, "Demo block");
        assert_eq!(catalog[0].wasm_sha256, "abcd");
        assert_eq!(catalog[0].wasm_size_bytes, 12);
        assert!(catalog[0].capabilities.contains("CAP_FS_READ"));
        assert!(catalog[0].capabilities.contains("CAP_NET_CONNECT"));
    }

    #[test]
    fn test_parse_array_index_still_works() {
        let text = serde_json::json!([{
            "name": "legacy",
            "version": "0.1.0",
            "description": "Legacy entry",
            "author": "t",
            "capabilities": [],
            "wasm_size_bytes": 1,
            "wasm_sha256": "abc"
        }])
        .to_string();
        let catalog = parse_index(&text).unwrap();
        assert_eq!(catalog[0].name, "legacy");
        assert_eq!(catalog[0].description, "Legacy entry");
    }

    #[test]
    fn test_local_nested_block_manifest_and_download() {
        let dir = tempfile::tempdir().unwrap();
        let block_dir = dir.path().join("blocks").join("gamma");
        std::fs::create_dir_all(&block_dir).unwrap();
        std::fs::write(block_dir.join("block.wasm"), b"nested-binary").unwrap();
        std::fs::write(
            block_dir.join("manifest.json"),
            serde_json::json!({
                "name": "gamma",
                "version": "3.0.0",
                "description": { "en": "Nested", "ru": "Вложенный" },
                "author": "nested-author",
                "capabilities": [],
                "wasm_hash": "sha256:placeholder",
                "wasm_size_bytes": 0
            })
            .to_string(),
        )
        .unwrap();

        let source = StoreSource::local(dir.path().to_str().unwrap());
        let catalog = fetch_index_local(&source).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "gamma");
        assert_eq!(catalog[0].version, "3.0.0");
        assert_eq!(catalog[0].description, "Nested");
        assert_eq!(catalog[0].author, "nested-author");
        assert_eq!(catalog[0].wasm_size_bytes, 13);
        assert_eq!(
            catalog[0].wasm_sha256,
            hex::encode(Sha256::digest(b"nested-binary"))
        );
        assert_eq!(
            download_block_local(&source, "gamma").unwrap(),
            b"nested-binary"
        );
    }

    #[test]
    fn test_object_index_enriches_hash_from_nested_binary() {
        let dir = tempfile::tempdir().unwrap();
        let block_dir = dir.path().join("blocks").join("demo");
        std::fs::create_dir_all(&block_dir).unwrap();
        std::fs::write(block_dir.join("block.wasm"), b"demo-bytes").unwrap();
        std::fs::write(
            dir.path().join("index.json"),
            serde_json::json!({
                "schema_version": "1.0",
                "blocks": [{
                    "name": "demo",
                    "version": "1.0.0",
                    "description": "Demo",
                    "author": "t",
                    "capabilities": [],
                    "wasm_hash": "sha256:placeholder",
                    "wasm_size_bytes": 0
                }]
            })
            .to_string(),
        )
        .unwrap();
        let source = StoreSource::local(dir.path().to_str().unwrap());
        let catalog = fetch_index_local(&source).unwrap();
        assert_eq!(catalog[0].wasm_size_bytes, 10);
        assert_eq!(
            catalog[0].wasm_sha256,
            hex::encode(Sha256::digest(b"demo-bytes"))
        );
    }
}
