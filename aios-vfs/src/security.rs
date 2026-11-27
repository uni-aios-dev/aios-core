use crate::error::{AIOSException, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// Capability token required for every `HOST://` read operation.
pub const HOST_READ_CAP: &str = "vfs:host:read";
/// Capability token required for every `HOST://` write operation.
pub const HOST_WRITE_CAP: &str = "vfs:host:write";
/// Path-scoped right checked before every read-like `AIOS://` operation.
pub const VFS_READ_CAP: &str = "vfs:read";
/// Path-scoped right checked before every write-like `AIOS://` operation.
pub const VFS_WRITE_CAP: &str = "vfs:write";

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

/// Normalized scheme-qualified path a path-scoped ACL rule applies to.
///
/// Bare paths default to the `AIOS` scheme; `AIOS://` and `HOST://` URIs keep
/// their scheme. The path component is normalized exactly like `VfsPath`, so
/// `..` can never widen a rule beyond the scheme root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathScope {
    /// Uppercase scheme name: `AIOS` or `HOST`.
    pub scheme: String,
    /// Normalized virtual path starting with `/`.
    pub path: String,
}

impl PathScope {
    /// Parse a URI (`AIOS:///sandbox`, `HOST:///Users`) or a bare virtual path.
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        let (scheme, rest) = if let Some(rest) = raw
            .strip_prefix("AIOS://")
            .or_else(|| raw.strip_prefix("aios://"))
        {
            ("AIOS", rest)
        } else if let Some(rest) = raw
            .strip_prefix("HOST://")
            .or_else(|| raw.strip_prefix("host://"))
        {
            ("HOST", rest)
        } else if raw.starts_with('/') {
            ("AIOS", raw)
        } else {
            return Err(AIOSException::InvalidPayload(format!(
                "invalid ACL path '{raw}' — expected AIOS://, HOST:// or an absolute path"
            )));
        };
        Ok(Self {
            scheme: scheme.to_string(),
            path: normalize_virtual_path(rest),
        })
    }

    /// Scope inside the sandboxed `AIOS://` scheme.
    pub fn aios(path: &str) -> Self {
        Self {
            scheme: "AIOS".to_string(),
            path: normalize_virtual_path(path),
        }
    }

    /// Scope inside the physical `HOST://` scheme.
    pub fn host(path: &str) -> Self {
        Self {
            scheme: "HOST".to_string(),
            path: normalize_virtual_path(path),
        }
    }

    /// Canonical URI form of the scope.
    pub fn to_uri(&self) -> String {
        format!("{}://{}", self.scheme, self.path)
    }

    /// Whether this scope covers `other`: same scheme and equal path or an
    /// ancestor directory. Root (`/`) covers the whole scheme.
    pub fn contains(&self, other: &Self) -> bool {
        if self.scheme != other.scheme {
            return false;
        }
        if self.path == "/" {
            return true;
        }
        self.path == other.path
            || other
                .path
                .starts_with(&format!("{}/", self.path.trim_end_matches('/')))
    }
}

/// One path-scoped rule: a right is explicitly allowed or denied on a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAclRule {
    /// Capability token the rule talks about (for example `vfs:read`).
    pub cap: String,
    /// Scheme-qualified path the rule applies to.
    pub scope: PathScope,
    /// `true` for a grant, `false` for a revocation.
    pub allowed: bool,
}

impl PathAclRule {
    /// Display form used by console output and diagnostics.
    pub fn display(&self) -> String {
        format!(
            "{} {} {}",
            if self.allowed { "grant" } else { "revoke" },
            self.cap,
            self.scope.to_uri()
        )
    }
}

/// Path-aware ACL consulted by `AiosVfs` after path canonicalization.
///
/// With no rules for a right, that right stays open on every sandboxed path,
/// preserving the original `AiosVfs` behaviour. As soon as a right has a rule,
/// access is explicit for that right: the most specific matching scope wins, a
/// revocation on a child overrides a grant on its parent, and a path with no
/// matching rule for that right is denied. Other rights are unaffected.
#[derive(Debug)]
pub struct PathAcl {
    rules: Mutex<Vec<PathAclRule>>,
}

impl Default for PathAcl {
    fn default() -> Self {
        Self::new()
    }
}

impl PathAcl {
    /// Create an empty policy (all sandboxed paths allowed).
    pub fn new() -> Self {
        Self {
            rules: Mutex::new(Vec::new()),
        }
    }

    /// Grant `cap` on `scope`, replacing a previous rule for the same pair.
    pub fn grant(&self, cap: &str, scope: &PathScope) -> Result<()> {
        self.set(cap, scope, true)
    }

    /// Deny `cap` on `scope`, overriding any grant inherited from an ancestor.
    pub fn revoke(&self, cap: &str, scope: &PathScope) -> Result<()> {
        self.set(cap, scope, false)
    }

    fn set(&self, cap: &str, scope: &PathScope, allowed: bool) -> Result<()> {
        Self::validate_cap(cap)?;
        let mut rules = self
            .rules
            .lock()
            .map_err(|_| AIOSException::Generic("path ACL lock poisoned".to_string()))?;
        rules.retain(|rule| rule.cap != cap || rule.scope != *scope);
        rules.push(PathAclRule {
            cap: cap.to_string(),
            scope: scope.clone(),
            allowed,
        });
        Ok(())
    }

    /// Reject empty or whitespace-containing capability tokens.
    pub fn validate_cap(cap: &str) -> Result<()> {
        if cap.is_empty() || cap.chars().any(char::is_whitespace) {
            return Err(AIOSException::InvalidPayload(format!(
                "invalid ACL capability '{cap}' — expected one non-empty token"
            )));
        }
        Ok(())
    }

    /// Whether `cap` is allowed on `scope` under the current policy.
    pub fn check(&self, cap: &str, scope: &PathScope) -> bool {
        let rules = match self.rules.lock() {
            Ok(rules) => rules,
            Err(_) => return false,
        };
        let mut best: Option<&PathAclRule> = None;
        for rule in rules.iter().filter(|rule| rule.cap == cap) {
            if !rule.scope.contains(scope) {
                continue;
            }
            let more_specific = match best {
                None => true,
                Some(current) => rule.scope.path.len() > current.scope.path.len(),
            };
            if more_specific {
                best = Some(rule);
            }
        }
        best.map(|rule| rule.allowed).unwrap_or(true)
    }

    /// Return `PermissionDenied` unless `cap` is allowed on `scope`.
    pub fn require(&self, cap: &str, scope: &PathScope) -> Result<()> {
        if self.check(cap, scope) {
            Ok(())
        } else {
            Err(AIOSException::PermissionDenied(format!(
                "path ACL denies '{cap}' on {}",
                scope.to_uri()
            )))
        }
    }

    /// Snapshot of all rules, sorted by capability, scheme and path.
    pub fn rules(&self) -> Vec<PathAclRule> {
        let mut out = self
            .rules
            .lock()
            .map(|rules| rules.clone())
            .unwrap_or_default();
        out.sort_by(|a, b| {
            (&a.cap, &a.scope.scheme, &a.scope.path).cmp(&(&b.cap, &b.scope.scheme, &b.scope.path))
        });
        out
    }

    /// Whether no rule has been configured yet.
    pub fn is_empty(&self) -> bool {
        self.rules
            .lock()
            .map(|rules| rules.is_empty())
            .unwrap_or(true)
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

    #[test]
    fn test_path_scope_parses_schemes_and_normalizes() {
        assert_eq!(
            PathScope::parse("AIOS:///sandbox/../config").unwrap(),
            PathScope::aios("/config")
        );
        assert_eq!(
            PathScope::parse("host://C:/Users").unwrap(),
            PathScope::host("/C:/Users")
        );
        assert_eq!(
            PathScope::parse("/sandbox").unwrap(),
            PathScope::aios("/sandbox")
        );
        assert!(PathScope::parse("relative/path").is_err());
        assert_eq!(PathScope::aios("/sandbox").to_uri(), "AIOS:///sandbox");
    }

    #[test]
    fn test_path_scope_contains_is_scheme_and_component_aware() {
        let parent = PathScope::aios("/sandbox");
        assert!(parent.contains(&PathScope::aios("/sandbox/app/file.txt")));
        assert!(!parent.contains(&PathScope::aios("/sandboxed/file.txt")));
        assert!(!parent.contains(&PathScope::host("/sandbox")));
        assert!(PathScope::aios("/").contains(&PathScope::aios("/store")));
    }

    #[test]
    fn test_path_acl_default_policy_allows_then_explicit_rules_deny() {
        let acl = PathAcl::new();
        let file = PathScope::aios("/sandbox/note.txt");
        assert!(acl.is_empty());
        assert!(acl.check(VFS_READ_CAP, &file));
        acl.revoke(VFS_READ_CAP, &PathScope::aios("/sandbox"))
            .unwrap();
        assert!(!acl.is_empty());
        assert!(!acl.check(VFS_READ_CAP, &file));
        assert!(acl.check(VFS_WRITE_CAP, &file));
        assert!(acl.require(VFS_READ_CAP, &file).is_err());
    }

    #[test]
    fn test_path_acl_most_specific_scope_wins() {
        let acl = PathAcl::new();
        acl.grant(VFS_WRITE_CAP, &PathScope::aios("/sandbox"))
            .unwrap();
        acl.revoke(VFS_WRITE_CAP, &PathScope::aios("/sandbox/secrets"))
            .unwrap();
        assert!(acl.check(VFS_WRITE_CAP, &PathScope::aios("/sandbox/app")));
        assert!(!acl.check(VFS_WRITE_CAP, &PathScope::aios("/sandbox/secrets/key")));
        acl.grant(VFS_WRITE_CAP, &PathScope::aios("/sandbox/secrets"))
            .unwrap();
        assert!(acl.check(VFS_WRITE_CAP, &PathScope::aios("/sandbox/secrets/key")));
        assert_eq!(acl.rules().len(), 2);
    }

    #[test]
    fn test_path_acl_rejects_invalid_capabilities() {
        let acl = PathAcl::new();
        assert!(acl.grant("", &PathScope::aios("/")).is_err());
        assert!(acl.revoke("two tokens", &PathScope::aios("/")).is_err());
        assert!(acl.is_empty());
    }
}
