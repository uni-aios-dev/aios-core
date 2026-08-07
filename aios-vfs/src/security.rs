use crate::error::{AIOSException, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// Capability token required for every `HOST://` read operation.
pub const HOST_READ_CAP: &str = "vfs:host:read";
/// Capability token required for every `HOST://` write operation.
pub const HOST_WRITE_CAP: &str = "vfs:host:write";

/// Capability ACL enforced by the kernel before any `HOST://` access.
///
/// The host scheme exposes physical disks, so every mount and every
/// read/write operation must be authorized with the appropriate token
/// (`vfs:host:read` / `vfs:host:write`). The `AIOS://` scheme is fully
/// sandboxed and never consults the ACL.
#[derive(Debug)]
pub struct AclContext {
    grants: Mutex<HashSet<String>>,
}

impl Clone for AclContext {
    fn clone(&self) -> Self {
        Self {
            grants: Mutex::new(self.grants.lock().map(|g| g.clone()).unwrap_or_default()),
        }
    }
}

impl Default for AclContext {
    fn default() -> Self {
        Self::new()
    }
}

impl AclContext {
    /// Create an empty ACL with no grants. No `HOST://` operation is possible
    /// until tokens are granted.
    pub fn new() -> Self {
        Self {
            grants: Mutex::new(HashSet::new()),
        }
    }

    /// Create an ACL pre-loaded with the given capability tokens.
    pub fn with_tokens(tokens: &[&str]) -> Self {
        let mut grants = HashSet::new();
        for t in tokens {
            grants.insert((*t).to_string());
        }
        Self {
            grants: Mutex::new(grants),
        }
    }

    /// Grant a capability token (e.g. `vfs:host:read`).
    pub fn grant(&self, token: &str) {
        if let Ok(mut grants) = self.grants.lock() {
            grants.insert(token.to_string());
        }
    }

    /// Revoke a previously granted capability token.
    pub fn revoke(&self, token: &str) {
        if let Ok(mut grants) = self.grants.lock() {
            grants.remove(token);
        }
    }

    /// Check whether the given capability token has been granted.
    pub fn has(&self, token: &str) -> bool {
        self.grants
            .lock()
            .map(|g| g.contains(token))
            .unwrap_or(false)
    }

    /// Snapshot of all granted capability tokens, sorted for stable display.
    pub fn tokens(&self) -> Vec<String> {
        let mut out = self
            .grants
            .lock()
            .map(|g| g.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Kernel check: reject the operation with `PermissionDenied` unless the
    /// token has been granted.
    pub fn require(&self, token: &str) -> Result<()> {
        if self.has(token) {
            Ok(())
        } else {
            Err(AIOSException::PermissionDenied(format!(
                "missing capability '{token}' — operation blocked by kernel ACL"
            )))
        }
    }
}

/// Normalize a virtual path string, collapsing `//`, removing `.`, resolving
/// `..` and clamping at the scheme root (so `..` can never escape `/`).
pub fn normalize_virtual_path(raw: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in raw.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    if out.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", out.join("/"))
    }
}

/// Resolve a candidate path and guarantee it stays inside `root`.
///
/// Two layers of defence are applied:
/// 1. a lexical walk over `candidate` relative to `root` that rejects any
///    `..` that would climb above `root`;
/// 2. if the file already exists, `std::fs::canonicalize` resolves symlinks
///    and the result is re-verified to still be inside `root`.
///
/// This is the Path Canonicalization gate that blocks sandbox escapes such as
/// `AIOS:///sandbox/../../../etc` from reaching the host filesystem.
pub fn canonicalize_inside(root: &Path, candidate: &Path) -> Result<PathBuf> {
    let rel = candidate.strip_prefix(root).map_err(|_| {
        AIOSException::PermissionDenied(format!(
            "path '{}' is outside the root '{}'",
            candidate.display(),
            root.display()
        ))
    })?;

    let mut stack: Vec<PathBuf> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.pop().is_none() {
                    return Err(AIOSException::PermissionDenied(format!(
                        "path '{}' escapes the root '{}'",
                        candidate.display(),
                        root.display()
                    )));
                }
            }
            Component::Normal(seg) => stack.push(PathBuf::from(seg)),
            Component::RootDir | Component::Prefix(_) => {
                return Err(AIOSException::PermissionDenied(format!(
                    "path '{}' contains an absolute component inside the root '{}'",
                    candidate.display(),
                    root.display()
                )));
            }
        }
    }

    let resolved = stack
        .iter()
        .fold(root.to_path_buf(), |acc, seg| acc.join(seg));

    if resolved.exists() {
        let canon = std::fs::canonicalize(&resolved)
            .map_err(|e| AIOSException::Generic(format!("canonicalize failed: {e}")))?;
        if !canon.starts_with(root) {
            return Err(AIOSException::PermissionDenied(format!(
                "symlink at '{}' escapes the root '{}'",
                resolved.display(),
                root.display()
            )));
        }
        return Ok(canon);
    }
    Ok(resolved)
}

/// Return the trailing file/dir name of a virtual path (last component).
pub fn path_file_name(virtual_path: &str) -> Option<String> {
    normalize_virtual_path(virtual_path)
        .split('/')
        .next_back()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Return the parent virtual path of a virtual path.
pub fn path_parent(virtual_path: &str) -> String {
    let norm = normalize_virtual_path(virtual_path);
    if norm == "/" {
        return "/".to_string();
    }
    let mut segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    segs.pop();
    if segs.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segs.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acl_rejects_without_token() {
        let acl = AclContext::new();
        assert!(acl.require(HOST_READ_CAP).is_err());
    }

    #[test]
    fn test_acl_grant_allows() {
        let acl = AclContext::with_tokens(&[HOST_READ_CAP]);
        assert!(acl.require(HOST_READ_CAP).is_ok());
        assert!(acl.require(HOST_WRITE_CAP).is_err());
    }

    #[test]
    fn test_acl_revoke() {
        let acl = AclContext::with_tokens(&[HOST_READ_CAP]);
        acl.revoke(HOST_READ_CAP);
        assert!(acl.require(HOST_READ_CAP).is_err());
    }

    #[test]
    fn test_normalize_virtual_path_clamps_escapes() {
        assert_eq!(normalize_virtual_path("/sandbox/.."), "/");
        assert_eq!(normalize_virtual_path("/../../etc"), "/etc");
        assert_eq!(normalize_virtual_path("/a//b/./c"), "/a/b/c");
        assert_eq!(normalize_virtual_path(""), "/");
        assert_eq!(normalize_virtual_path("foo\\bar"), "/foo/bar");
    }

    #[test]
    fn test_canonicalize_inside_rejects_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let escape = root.join("..").join("outside");
        assert!(canonicalize_inside(root, &escape).is_err());
    }

    #[test]
    fn test_canonicalize_inside_accepts_inside() {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize the root first (on Windows `canonicalize` returns a
        // verbatim `\\?\` path; both operands must share the same form).
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        let candidate = root.join("a").join("b");
        assert!(canonicalize_inside(&root, &candidate).is_ok());
    }

    #[test]
    fn test_canonicalize_inside_absolute_component_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let candidate = root.join("ok").join("..").join("..").join("..");
        assert!(canonicalize_inside(root, &candidate).is_err());
    }

    #[test]
    fn test_path_parent_and_file_name() {
        assert_eq!(path_parent("/sandbox/sub"), "/sandbox");
        assert_eq!(path_parent("/sandbox"), "/");
        assert_eq!(path_parent("/"), "/");
        assert_eq!(
            path_file_name("/sandbox/file.txt").as_deref(),
            Some("file.txt")
        );
        assert_eq!(path_file_name("/"), None);
    }
}
