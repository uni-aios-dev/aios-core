use crate::error::{AIOSException, Result};
use crate::security::{
    canonicalize_inside, normalize_virtual_path, path_file_name, path_parent, AclContext,
    HOST_READ_CAP, HOST_WRITE_CAP,
};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::io::{AsyncRead, AsyncSeek, AsyncWrite};

/// A seekable async reader: combines `AsyncRead` and `AsyncSeek` so a file
/// handle can be both positioned (`seek`) and streamed (`read`).
pub trait AsyncSeekReader: AsyncRead + AsyncSeek {}

impl<T: AsyncRead + AsyncSeek> AsyncSeekReader for T {}

/// The two addressing schemes supported by the VFS:
///
/// * `AIOS://` — the isolated virtual filesystem inside the sandbox
///   (application root, `/system`, `/sandbox`, `/store`, `/config`);
/// * `HOST://` — direct access to the physical host disks, guarded by the
///   capability ACL (`vfs:host:read` / `vfs:host:write`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VfsScheme {
    Aios,
    Host,
}

impl VfsScheme {
    /// URI scheme prefix, e.g. `AIOS://` or `HOST://`.
    pub fn as_uri(&self) -> &'static str {
        match self {
            VfsScheme::Aios => "AIOS://",
            VfsScheme::Host => "HOST://",
        }
    }
}

impl std::fmt::Display for VfsScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_uri())
    }
}

/// A fully qualified URI inside the VFS, e.g. `AIOS:///sandbox/file.txt`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VfsPath {
    /// The addressing scheme.
    pub scheme: VfsScheme,
    /// The normalized virtual path (always starts with `/`, never ends with
    /// one except the root itself).
    pub path: String,
}

impl VfsPath {
    /// Parse a URI like `AIOS:///sandbox`, `HOST:///C:/Users` or
    /// `HOST://C:/Users`. Scheme matching is case-insensitive; the path is
    /// normalized and `..` is clamped at the scheme root.
    pub fn parse(uri: &str) -> Result<VfsPath> {
        let uri = uri.trim();
        let (scheme, rest) = if let Some(rest) = uri
            .strip_prefix("AIOS://")
            .or_else(|| uri.strip_prefix("aios://"))
        {
            (VfsScheme::Aios, rest)
        } else if let Some(rest) = uri
            .strip_prefix("HOST://")
            .or_else(|| uri.strip_prefix("host://"))
        {
            (VfsScheme::Host, rest)
        } else {
            return Err(AIOSException::InvalidPayload(format!(
                "invalid VFS URI '{uri}' — expected AIOS:// or HOST://"
            )));
        };
        Ok(VfsPath {
            scheme,
            path: normalize_virtual_path(rest),
        })
    }

    /// Render the canonical URI form.
    pub fn to_uri(&self) -> String {
        format!("{}{}", self.scheme.as_uri(), self.path)
    }

    /// Join a single path segment (a file or directory name).
    pub fn join(&self, name: &str) -> VfsPath {
        VfsPath {
            scheme: self.scheme,
            path: normalize_virtual_path(&format!("{}/{}", self.path, name)),
        }
    }

    /// Parent directory. The root is its own parent.
    pub fn parent(&self) -> VfsPath {
        VfsPath {
            scheme: self.scheme,
            path: path_parent(&self.path),
        }
    }

    /// Trailing component name, if any (root has none).
    pub fn file_name(&self) -> Option<String> {
        path_file_name(&self.path)
    }

    /// Whether this is the scheme root (`/`).
    pub fn is_root(&self) -> bool {
        self.path == "/"
    }
}

impl std::fmt::Display for VfsPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_uri())
    }
}

/// One row of a directory listing.
#[derive(Debug, Clone)]
pub struct VfsEntry {
    /// Entry name (not the full path).
    pub name: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// File size in bytes (0 for directories).
    pub size: u64,
    /// Last modification time, if the underlying filesystem reports one.
    pub modified: Option<SystemTime>,
    /// Human-readable permission string (`rwx` for the owner).
    pub permissions: String,
    /// Capability tokens required to touch the entry on `HOST://`
    /// (empty on `AIOS://`, which is fully sandboxed).
    pub acl: Vec<String>,
}

/// Metadata for a single VFS object.
#[derive(Debug, Clone)]
pub struct VfsMetadata {
    /// Whether the object is a directory.
    pub is_dir: bool,
    /// Whether the object is a regular file.
    pub is_file: bool,
    /// Whether the object is a symbolic link.
    pub is_symlink: bool,
    /// Size in bytes.
    pub size: u64,
    /// Last modification time.
    pub modified: Option<SystemTime>,
    /// Whether the object is read-only.
    pub readonly: bool,
    /// Permission string.
    pub permissions: String,
    /// Capability tokens required for `HOST://` access.
    pub acl: Vec<String>,
}

/// The abstract async filesystem contract. Every operation is fully
/// asynchronous (`tokio::fs`), capability-checked for `HOST://`, and
/// canonicalization-guarded for `AIOS://`.
#[async_trait]
pub trait VirtualFileSystem: Send + Sync {
    /// Which addressing scheme this instance serves.
    fn scheme(&self) -> VfsScheme;

    /// Capability ACL used for `HOST://` operations.
    fn acl(&self) -> &AclContext;

    /// List a directory, sorted (directories first, then case-insensitive).
    async fn list_dir(&self, dir: &VfsPath) -> Result<Vec<VfsEntry>>;

    /// Read metadata for a single object.
    async fn metadata(&self, path: &VfsPath) -> Result<VfsMetadata>;

    /// Read the whole file into memory.
    async fn read_file(&self, path: &VfsPath) -> Result<Vec<u8>>;

    /// Write a whole file (creating parents as needed).
    async fn write_file(&self, path: &VfsPath, data: &[u8]) -> Result<()>;

    /// Atomically write a file through a temporary sibling + rename.
    async fn atomic_write(&self, path: &VfsPath, data: &[u8]) -> Result<()>;

    /// Create a directory (and any missing parents).
    async fn create_dir(&self, path: &VfsPath) -> Result<()>;

    /// Delete a single object (recursion is handled by `operations`).
    async fn delete_item(&self, path: &VfsPath) -> Result<()>;

    /// Rename/move within the same scheme.
    async fn rename(&self, from: &VfsPath, to: &VfsPath) -> Result<()>;

    /// Whether the object exists.
    async fn exists(&self, path: &VfsPath) -> Result<bool>;

    /// Open a file for sequential reading.
    async fn open_read(&self, path: &VfsPath) -> Result<Box<dyn AsyncRead + Send + Unpin>>;

    /// Open a file for writing (created/truncated).
    async fn open_write(&self, path: &VfsPath) -> Result<Box<dyn AsyncWrite + Send + Unpin>>;

    /// Open a file with random access for seeking and reading.
    async fn open_seek(&self, path: &VfsPath) -> Result<Box<dyn AsyncSeekReader + Send + Unpin>>;

    /// Resolve a `VfsPath` to the physical host path after ACL checks and
    /// path canonicalization.
    async fn resolve(&self, path: &VfsPath) -> Result<PathBuf>;
}

/// Isolated virtual filesystem backing the `AIOS://` scheme.
///
/// All paths live under a single physical sandbox root and are laid out as
/// `/system`, `/sandbox`, `/store`, `/config`. Escape attempts (`..` or a
/// symlink pointing outside) are rejected by `canonicalize_inside`. No
/// capability token is needed: the whole tree is the sandbox.
#[derive(Debug)]
pub struct AiosVfs {
    root: PathBuf,
    acl: AclContext,
}

impl AiosVfs {
    const SUBDIRS: [&'static str; 4] = ["system", "sandbox", "store", "config"];

    /// Create (or re-open) the sandbox under `root`, creating the standard
    /// subdirectories. `root` is canonicalized so escape checks are exact.
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)
            .map_err(|e| AIOSException::Generic(format!("create sandbox root failed: {e}")))?;
        for sub in Self::SUBDIRS {
            std::fs::create_dir_all(root.join(sub)).map_err(|e| {
                AIOSException::Generic(format!("create sandbox dir '{sub}' failed: {e}"))
            })?;
        }
        let canonical = std::fs::canonicalize(&root).map_err(|e| {
            AIOSException::Generic(format!("canonicalize sandbox root failed: {e}"))
        })?;
        Ok(Self {
            root: canonical,
            acl: AclContext::new(),
        })
    }

    /// Physical sandbox root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    async fn require_aios_path(&self, path: &VfsPath) -> Result<PathBuf> {
        if path.scheme != VfsScheme::Aios {
            return Err(AIOSException::InvalidPayload(format!(
                "path '{}' does not belong to the AIOS scheme",
                path.to_uri()
            )));
        }
        let mut full = self.root.clone();
        for seg in path.path.split('/').filter(|s| !s.is_empty()) {
            full.push(seg);
        }
        canonicalize_inside(&self.root, &full)
    }
}

#[async_trait]
impl VirtualFileSystem for AiosVfs {
    fn scheme(&self) -> VfsScheme {
        VfsScheme::Aios
    }

    fn acl(&self) -> &AclContext {
        &self.acl
    }

    async fn list_dir(&self, dir: &VfsPath) -> Result<Vec<VfsEntry>> {
        let full = self.require_aios_path(dir).await?;
        let mut out = Vec::new();
        let mut rd = tokio::fs::read_dir(&full).await.map_err(|e| {
            AIOSException::Generic(format!("read_dir '{}' failed: {e}", full.display()))
        })?;
        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| AIOSException::Generic(format!("read_dir entry failed: {e}")))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| AIOSException::Generic(format!("file_type failed: {e}")))?;
            let md = entry
                .metadata()
                .await
                .map_err(|e| AIOSException::Generic(format!("metadata failed: {e}")))?;
            out.push(VfsEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: file_type.is_dir(),
                size: md.len(),
                modified: md.modified().ok(),
                permissions: format_permissions(&md),
                acl: Vec::new(),
            });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    async fn metadata(&self, path: &VfsPath) -> Result<VfsMetadata> {
        let full = self.require_aios_path(path).await?;
        let md = tokio::fs::metadata(&full).await.map_err(|e| {
            AIOSException::Generic(format!("metadata '{}' failed: {e}", full.display()))
        })?;
        Ok(metadata_from_std(&md))
    }

    async fn read_file(&self, path: &VfsPath) -> Result<Vec<u8>> {
        let full = self.require_aios_path(path).await?;
        tokio::fs::read(&full)
            .await
            .map_err(|e| AIOSException::Generic(format!("read '{}' failed: {e}", full.display())))
    }

    async fn write_file(&self, path: &VfsPath, data: &[u8]) -> Result<()> {
        let full = self.require_aios_path(path).await?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AIOSException::Generic(format!("create_dir_all failed: {e}")))?;
        }
        tokio::fs::write(&full, data)
            .await
            .map_err(|e| AIOSException::Generic(format!("write '{}' failed: {e}", full.display())))
    }

    async fn atomic_write(&self, path: &VfsPath, data: &[u8]) -> Result<()> {
        let full = self.require_aios_path(path).await?;
        atomic_write_impl(&full, data).await
    }

    async fn create_dir(&self, path: &VfsPath) -> Result<()> {
        let full = self.require_aios_path(path).await?;
        tokio::fs::create_dir_all(&full).await.map_err(|e| {
            AIOSException::Generic(format!("create_dir '{}' failed: {e}", full.display()))
        })
    }

    async fn delete_item(&self, path: &VfsPath) -> Result<()> {
        let full = self.require_aios_path(path).await?;
        let md = tokio::fs::metadata(&full)
            .await
            .map_err(|e| AIOSException::Generic(format!("metadata failed: {e}")))?;
        if md.is_dir() {
            tokio::fs::remove_dir(&full).await.map_err(|e| {
                AIOSException::Generic(format!("remove_dir '{}' failed: {e}", full.display()))
            })
        } else {
            tokio::fs::remove_file(&full).await.map_err(|e| {
                AIOSException::Generic(format!("remove_file '{}' failed: {e}", full.display()))
            })
        }
    }

    async fn rename(&self, from: &VfsPath, to: &VfsPath) -> Result<()> {
        let from_full = self.require_aios_path(from).await?;
        let to_full = self.require_aios_path(to).await?;
        tokio::fs::rename(&from_full, &to_full)
            .await
            .map_err(|e| AIOSException::Generic(format!("rename failed: {e}")))
    }

    async fn exists(&self, path: &VfsPath) -> Result<bool> {
        let full = self.require_aios_path(path).await?;
        Ok(full.exists())
    }

    async fn open_read(&self, path: &VfsPath) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        let full = self.require_aios_path(path).await?;
        let f = tokio::fs::File::open(&full).await.map_err(|e| {
            AIOSException::Generic(format!("open '{}' failed: {e}", full.display()))
        })?;
        Ok(Box::new(f))
    }

    async fn open_write(&self, path: &VfsPath) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        let full = self.require_aios_path(path).await?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AIOSException::Generic(format!("create_dir_all failed: {e}")))?;
        }
        let f = tokio::fs::File::create(&full).await.map_err(|e| {
            AIOSException::Generic(format!("create '{}' failed: {e}", full.display()))
        })?;
        Ok(Box::new(f))
    }

    async fn open_seek(&self, path: &VfsPath) -> Result<Box<dyn AsyncSeekReader + Send + Unpin>> {
        let full = self.require_aios_path(path).await?;
        let f = tokio::fs::File::open(&full).await.map_err(|e| {
            AIOSException::Generic(format!("open '{}' failed: {e}", full.display()))
        })?;
        Ok(Box::new(f))
    }

    async fn resolve(&self, path: &VfsPath) -> Result<PathBuf> {
        self.require_aios_path(path).await
    }
}

/// Physical host disks backed by the `HOST://` scheme. Every operation is
/// gated on the kernel capability ACL (`vfs:host:read` / `vfs:host:write`).
#[derive(Debug)]
pub struct HostVfs {
    root: PathBuf,
    acl: AclContext,
}

/// Default physical root: on Windows the current drive (`C:\`), on Unix `/`.
#[cfg(windows)]
pub fn default_host_root() -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|c| c.ancestors().last().map(|r| r.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("C:\\"))
}

/// Default physical root: on Unix the filesystem root `/`.
#[cfg(not(windows))]
pub fn default_host_root() -> PathBuf {
    PathBuf::from("/")
}

impl HostVfs {
    /// Create a host filesystem over `root` (defaults to the filesystem
    /// root). The ACL decides which capabilities are allowed; with an empty
    /// ACL every `HOST://` operation is denied.
    pub fn new(root: Option<PathBuf>, acl: AclContext) -> Result<Self> {
        let root = root.unwrap_or_else(default_host_root);
        let canonical = std::fs::canonicalize(&root)
            .or_else(|_| std::fs::create_dir_all(&root).and_then(|_| std::fs::canonicalize(&root)))
            .map_err(|e| AIOSException::Generic(format!("host root unavailable: {e}")))?;
        Ok(Self {
            root: canonical,
            acl,
        })
    }

    /// Physical root on disk.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a host path. A leading drive segment such as `C:` switches the
    /// base to that drive; otherwise the default root is used. Path
    /// canonicalization guarantees no escape above the base.
    async fn require_host_path(&self, path: &VfsPath) -> Result<PathBuf> {
        if path.scheme != VfsScheme::Host {
            return Err(AIOSException::InvalidPayload(format!(
                "path '{}' does not belong to the HOST scheme",
                path.to_uri()
            )));
        }
        let segs: Vec<&str> = path.path.split('/').filter(|s| !s.is_empty()).collect();
        let mut base = self.root.clone();
        let rest: &[&str] = if let Some((drive, tail)) = segs.split_first() {
            let is_drive = drive.len() == 2
                && drive.as_bytes()[1] == b':'
                && drive.as_bytes()[0].is_ascii_alphabetic();
            if is_drive {
                base = PathBuf::from(format!("{}\\", drive));
                tail
            } else {
                segs.as_slice()
            }
        } else {
            segs.as_slice()
        };
        let mut full = base.clone();
        for seg in rest {
            full.push(seg);
        }
        canonicalize_inside(&base, &full)
    }
}

#[async_trait]
impl VirtualFileSystem for HostVfs {
    fn scheme(&self) -> VfsScheme {
        VfsScheme::Host
    }

    fn acl(&self) -> &AclContext {
        &self.acl
    }

    async fn list_dir(&self, dir: &VfsPath) -> Result<Vec<VfsEntry>> {
        self.acl.require(HOST_READ_CAP)?;
        let full = self.require_host_path(dir).await?;
        let mut out = Vec::new();
        let mut rd = tokio::fs::read_dir(&full).await.map_err(|e| {
            AIOSException::Generic(format!("read_dir '{}' failed: {e}", full.display()))
        })?;
        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| AIOSException::Generic(format!("read_dir entry failed: {e}")))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| AIOSException::Generic(format!("file_type failed: {e}")))?;
            let md = entry
                .metadata()
                .await
                .map_err(|e| AIOSException::Generic(format!("metadata failed: {e}")))?;
            out.push(VfsEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: file_type.is_dir(),
                size: md.len(),
                modified: md.modified().ok(),
                permissions: format_permissions(&md),
                acl: self.acl.tokens(),
            });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    async fn metadata(&self, path: &VfsPath) -> Result<VfsMetadata> {
        self.acl.require(HOST_READ_CAP)?;
        let full = self.require_host_path(path).await?;
        let md = tokio::fs::metadata(&full).await.map_err(|e| {
            AIOSException::Generic(format!("metadata '{}' failed: {e}", full.display()))
        })?;
        let mut meta = metadata_from_std(&md);
        meta.acl = self.acl.tokens();
        Ok(meta)
    }

    async fn read_file(&self, path: &VfsPath) -> Result<Vec<u8>> {
        self.acl.require(HOST_READ_CAP)?;
        let full = self.require_host_path(path).await?;
        tokio::fs::read(&full)
            .await
            .map_err(|e| AIOSException::Generic(format!("read '{}' failed: {e}", full.display())))
    }

    async fn write_file(&self, path: &VfsPath, data: &[u8]) -> Result<()> {
        self.acl.require(HOST_WRITE_CAP)?;
        let full = self.require_host_path(path).await?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AIOSException::Generic(format!("create_dir_all failed: {e}")))?;
        }
        tokio::fs::write(&full, data)
            .await
            .map_err(|e| AIOSException::Generic(format!("write '{}' failed: {e}", full.display())))
    }

    async fn atomic_write(&self, path: &VfsPath, data: &[u8]) -> Result<()> {
        self.acl.require(HOST_WRITE_CAP)?;
        let full = self.require_host_path(path).await?;
        atomic_write_impl(&full, data).await
    }

    async fn create_dir(&self, path: &VfsPath) -> Result<()> {
        self.acl.require(HOST_WRITE_CAP)?;
        let full = self.require_host_path(path).await?;
        tokio::fs::create_dir_all(&full).await.map_err(|e| {
            AIOSException::Generic(format!("create_dir '{}' failed: {e}", full.display()))
        })
    }

    async fn delete_item(&self, path: &VfsPath) -> Result<()> {
        self.acl.require(HOST_WRITE_CAP)?;
        let full = self.require_host_path(path).await?;
        let md = tokio::fs::metadata(&full)
            .await
            .map_err(|e| AIOSException::Generic(format!("metadata failed: {e}")))?;
        if md.is_dir() {
            tokio::fs::remove_dir(&full).await.map_err(|e| {
                AIOSException::Generic(format!("remove_dir '{}' failed: {e}", full.display()))
            })
        } else {
            tokio::fs::remove_file(&full).await.map_err(|e| {
                AIOSException::Generic(format!("remove_file '{}' failed: {e}", full.display()))
            })
        }
    }

    async fn rename(&self, from: &VfsPath, to: &VfsPath) -> Result<()> {
        self.acl.require(HOST_WRITE_CAP)?;
        let from_full = self.require_host_path(from).await?;
        let to_full = self.require_host_path(to).await?;
        tokio::fs::rename(&from_full, &to_full)
            .await
            .map_err(|e| AIOSException::Generic(format!("rename failed: {e}")))
    }

    async fn exists(&self, path: &VfsPath) -> Result<bool> {
        self.acl.require(HOST_READ_CAP)?;
        let full = self.require_host_path(path).await?;
        Ok(full.exists())
    }

    async fn open_read(&self, path: &VfsPath) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        self.acl.require(HOST_READ_CAP)?;
        let full = self.require_host_path(path).await?;
        let f = tokio::fs::File::open(&full).await.map_err(|e| {
            AIOSException::Generic(format!("open '{}' failed: {e}", full.display()))
        })?;
        Ok(Box::new(f))
    }

    async fn open_write(&self, path: &VfsPath) -> Result<Box<dyn AsyncWrite + Send + Unpin>> {
        self.acl.require(HOST_WRITE_CAP)?;
        let full = self.require_host_path(path).await?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AIOSException::Generic(format!("create_dir_all failed: {e}")))?;
        }
        let f = tokio::fs::File::create(&full).await.map_err(|e| {
            AIOSException::Generic(format!("create '{}' failed: {e}", full.display()))
        })?;
        Ok(Box::new(f))
    }

    async fn open_seek(&self, path: &VfsPath) -> Result<Box<dyn AsyncSeekReader + Send + Unpin>> {
        self.acl.require(HOST_READ_CAP)?;
        let full = self.require_host_path(path).await?;
        let f = tokio::fs::File::open(&full).await.map_err(|e| {
            AIOSException::Generic(format!("open '{}' failed: {e}", full.display()))
        })?;
        Ok(Box::new(f))
    }

    async fn resolve(&self, path: &VfsPath) -> Result<PathBuf> {
        self.acl.require(HOST_READ_CAP)?;
        self.require_host_path(path).await
    }
}

/// Build a `VfsMetadata` from `std::fs::Metadata`.
fn metadata_from_std(md: &std::fs::Metadata) -> VfsMetadata {
    VfsMetadata {
        is_dir: md.is_dir(),
        is_file: md.is_file(),
        is_symlink: md.file_type().is_symlink(),
        size: md.len(),
        modified: md.modified().ok(),
        readonly: md.permissions().readonly(),
        permissions: format_permissions(md),
        acl: Vec::new(),
    }
}

/// Render a permission string for the owner: `rwx` / `r--` etc.
fn format_permissions(md: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = md.permissions().mode();
        let mut s = String::with_capacity(3);
        s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
        s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
        s.push(if mode & 0o100 != 0 { 'x' } else { '-' });
        s
    }
    #[cfg(not(unix))]
    {
        if md.permissions().readonly() {
            "r--".to_string()
        } else {
            "rw-".to_string()
        }
    }
}

/// Sort entries: directories first, then case-insensitive by name.
fn sort_entries(entries: &mut [VfsEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Atomic write helper: write to a sibling `*.tmp` file, fsync, then rename
/// over the destination (removing a stale destination first on Windows).
async fn atomic_write_impl(full: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AIOSException::Generic(format!("create_dir_all failed: {e}")))?;
    }
    let file_name = full
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".into());
    let tmp = full.with_file_name(format!("{}.tmp", file_name));
    tokio::fs::write(&tmp, data).await.map_err(|e| {
        AIOSException::Generic(format!("tmp write '{}' failed: {e}", tmp.display()))
    })?;
    tokio::fs::remove_file(full).await.ok();
    tokio::fs::rename(&tmp, full)
        .await
        .map_err(|e| AIOSException::Generic(format!("atomic rename failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vfs_path_parse_aios() {
        let p = VfsPath::parse("AIOS:///sandbox").unwrap();
        assert_eq!(p.scheme, VfsScheme::Aios);
        assert_eq!(p.path, "/sandbox");
        assert_eq!(p.to_uri(), "AIOS:///sandbox");
    }

    #[test]
    fn test_vfs_path_parse_host_lowercase() {
        let p = VfsPath::parse("host://C:/Users").unwrap();
        assert_eq!(p.scheme, VfsScheme::Host);
        assert_eq!(p.path, "/C:/Users");
    }

    #[test]
    fn test_vfs_path_parse_invalid() {
        assert!(VfsPath::parse("ftp://x").is_err());
    }

    #[test]
    fn test_vfs_path_join_and_parent() {
        let base = VfsPath::parse("AIOS:///sandbox").unwrap();
        let child = base.join("app.log");
        assert_eq!(child.to_uri(), "AIOS:///sandbox/app.log");
        assert_eq!(child.parent(), base);
        assert_eq!(child.file_name().as_deref(), Some("app.log"));
        assert!(!base.is_root());
    }

    #[test]
    fn test_vfs_path_root_is_its_own_parent() {
        let root = VfsPath::parse("AIOS://").unwrap();
        assert!(root.is_root());
        assert_eq!(root.parent(), root);
    }

    #[tokio::test]
    async fn test_aios_sandbox_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = AiosVfs::new(dir.path().to_path_buf()).unwrap();
        let sub = VfsPath::parse("AIOS:///sandbox").unwrap();
        let entries = vfs.list_dir(&sub).await.unwrap();
        assert!(entries.is_empty());

        let file = sub.join("hello.txt");
        vfs.write_file(&file, b"hello world").await.unwrap();
        assert!(vfs.exists(&file).await.unwrap());
        assert_eq!(vfs.read_file(&file).await.unwrap(), b"hello world");
        vfs.delete_item(&file).await.unwrap();
        assert!(!vfs.exists(&file).await.unwrap());
    }

    #[tokio::test]
    async fn test_aios_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = AiosVfs::new(dir.path().to_path_buf()).unwrap();
        let cfg = VfsPath::parse("AIOS:///config/settings.json").unwrap();
        vfs.atomic_write(&cfg, br#"{"a":1}"#).await.unwrap();
        assert_eq!(vfs.read_file(&cfg).await.unwrap(), br#"{"a":1}"#);
        assert!(!cfg.to_uri().replace(".json", ".json.tmp").is_empty());
    }

    #[tokio::test]
    async fn test_host_requires_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let denied = HostVfs::new(Some(dir.path().to_path_buf()), AclContext::new()).unwrap();
        let root = VfsPath::parse("HOST:///").unwrap();
        assert!(denied.list_dir(&root).await.is_err());
        assert!(denied.write_file(&root.join("x.txt"), b"x").await.is_err());

        let allowed = HostVfs::new(
            Some(dir.path().to_path_buf()),
            AclContext::with_tokens(&["vfs:host:read"]),
        )
        .unwrap();
        assert!(allowed.list_dir(&root).await.is_ok());
        assert!(allowed.write_file(&root.join("x.txt"), b"x").await.is_err());
    }

    #[tokio::test]
    async fn test_aios_escape_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = AiosVfs::new(dir.path().to_path_buf()).unwrap();
        let evil = VfsPath::parse("AIOS:///sandbox/../../../etc/passwd").unwrap();
        assert!(vfs.read_file(&evil).await.is_err());
    }
}
