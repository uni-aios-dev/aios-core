use crate::error::{AIOSException, Result};
use crate::vfs::{VfsEntry, VfsPath, VirtualFileSystem};
use std::io::SeekFrom;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const COPY_BUF_SIZE: usize = 64 * 1024;

/// Cooperative cancellation token shared with a running background operation.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// A fresh, un-cancelled token.
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation; checked cooperatively between chunks.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Progress tracker for background copy/move/delete operations. Values are
/// atomics so the UI can sample them from another thread while the operation
/// runs on the tokio worker.
#[derive(Debug, Clone)]
pub struct Progress {
    total: Arc<AtomicU64>,
    done: Arc<AtomicU64>,
    cancelled: CancellationToken,
}

impl Progress {
    /// A new tracker with zeroed counters and a cancellable token.
    pub fn new() -> Self {
        Self {
            total: Arc::new(AtomicU64::new(0)),
            done: Arc::new(AtomicU64::new(0)),
            cancelled: CancellationToken::new(),
        }
    }

    /// Set the expected total (bytes to process).
    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    /// Report `n` bytes processed.
    pub fn add_done(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// Total bytes expected.
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Bytes processed so far.
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// Completion fraction in `0.0..=1.0` (1.0 when total is zero).
    pub fn fraction(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            1.0
        } else {
            (self.done() as f64 / total as f64).min(1.0)
        }
    }

    /// Request cancellation of the associated operation.
    pub fn cancel(&self) {
        self.cancelled.cancel();
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }

    /// A clone of the cancellation token.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancelled.clone()
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a copy/move operation.
#[derive(Debug, Clone, Default)]
pub struct CopyStats {
    /// Files copied/moved.
    pub files: u64,
    /// Directories created.
    pub dirs: u64,
    /// Bytes transferred.
    pub bytes: u64,
}

/// Result of a delete operation.
#[derive(Debug, Clone, Default)]
pub struct DeleteStats {
    /// Files deleted.
    pub files: u64,
    /// Directories deleted.
    pub dirs: u64,
    /// Bytes reclaimed.
    pub bytes: u64,
}

fn cancelled_error() -> AIOSException {
    AIOSException::Generic("operation cancelled by user".into())
}

/// List a directory (convenience wrapper over the trait).
pub async fn list_dir(fs: &dyn VirtualFileSystem, dir: &VfsPath) -> Result<Vec<VfsEntry>> {
    fs.list_dir(dir).await
}

/// Pre-scan the total size in bytes of everything under `path` (recursive).
pub async fn total_bytes(fs: &dyn VirtualFileSystem, path: &VfsPath) -> Result<u64> {
    let meta = fs.metadata(path).await?;
    if !meta.is_dir {
        return Ok(meta.size);
    }
    let mut total = 0u64;
    let entries = fs.list_dir(path).await?;
    for entry in entries {
        total += Box::pin(total_bytes(fs, &path.join(&entry.name))).await?;
    }
    Ok(total)
}

/// Recursively copy `src` to `dst`, possibly across schemes, streaming via
/// `AsyncRead`/`AsyncWrite` in chunks and updating `progress` continuously.
pub async fn copy_recursive(
    src_fs: &dyn VirtualFileSystem,
    dst_fs: &dyn VirtualFileSystem,
    src: &VfsPath,
    dst: &VfsPath,
    progress: &Progress,
) -> Result<CopyStats> {
    if progress.is_cancelled() {
        return Err(cancelled_error());
    }
    let meta = src_fs.metadata(src).await?;
    let mut stats = CopyStats::default();
    if meta.is_dir {
        dst_fs.create_dir(dst).await?;
        stats.dirs += 1;
        let entries = src_fs.list_dir(src).await?;
        for entry in entries {
            if progress.is_cancelled() {
                return Err(cancelled_error());
            }
            let sub = Box::pin(copy_recursive(
                src_fs,
                dst_fs,
                &src.join(&entry.name),
                &dst.join(&entry.name),
                progress,
            ))
            .await?;
            stats.files += sub.files;
            stats.dirs += sub.dirs;
            stats.bytes += sub.bytes;
        }
    } else {
        let mut reader = src_fs.open_read(src).await?;
        let mut writer = dst_fs.open_write(dst).await?;
        let mut buf = vec![0u8; COPY_BUF_SIZE];
        loop {
            if progress.is_cancelled() {
                return Err(cancelled_error());
            }
            let n = reader
                .read(&mut buf)
                .await
                .map_err(|e| AIOSException::Generic(format!("copy read failed: {e}")))?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .await
                .map_err(|e| AIOSException::Generic(format!("copy write failed: {e}")))?;
            progress.add_done(n as u64);
            stats.bytes += n as u64;
        }
        writer
            .flush()
            .await
            .map_err(|e| AIOSException::Generic(format!("copy flush failed: {e}")))?;
        stats.files += 1;
    }
    Ok(stats)
}

/// Move `src` to `dst`. Within one scheme a rename is attempted first; across
/// schemes (or when rename fails) a copy + delete is performed.
pub async fn move_item(
    src_fs: &dyn VirtualFileSystem,
    dst_fs: &dyn VirtualFileSystem,
    src: &VfsPath,
    dst: &VfsPath,
    progress: &Progress,
) -> Result<CopyStats> {
    if src.scheme == dst.scheme {
        if let Ok(()) = dst_fs.rename(src, dst).await {
            return Ok(CopyStats {
                files: 1,
                dirs: 0,
                bytes: 0,
            });
        }
    }
    let stats = copy_recursive(src_fs, dst_fs, src, dst, progress).await?;
    delete_item(src_fs, src, progress).await?;
    Ok(stats)
}

/// Recursively delete `path`, updating `progress`.
pub async fn delete_item(
    fs: &dyn VirtualFileSystem,
    path: &VfsPath,
    progress: &Progress,
) -> Result<DeleteStats> {
    if progress.is_cancelled() {
        return Err(cancelled_error());
    }
    let meta = fs.metadata(path).await?;
    let mut stats = DeleteStats::default();
    if meta.is_dir {
        let entries = fs.list_dir(path).await?;
        for entry in entries {
            if progress.is_cancelled() {
                return Err(cancelled_error());
            }
            let sub = Box::pin(delete_item(fs, &path.join(&entry.name), progress)).await?;
            stats.files += sub.files;
            stats.dirs += sub.dirs;
            stats.bytes += sub.bytes;
        }
        fs.delete_item(path).await?;
        stats.dirs += 1;
    } else {
        fs.delete_item(path).await?;
        progress.add_done(meta.size);
        stats.files += 1;
        stats.bytes += meta.size;
    }
    Ok(stats)
}

/// Read up to `max` bytes from the start of a file (streaming, bounded).
pub async fn read_head(fs: &dyn VirtualFileSystem, path: &VfsPath, max: usize) -> Result<Vec<u8>> {
    let mut reader = fs.open_read(path).await?;
    let mut buf = vec![0u8; max];
    let mut got = 0usize;
    while got < max {
        let n = reader
            .read(&mut buf[got..])
            .await
            .map_err(|e| AIOSException::Generic(format!("read failed: {e}")))?;
        if n == 0 {
            break;
        }
        got += n;
    }
    buf.truncate(got);
    Ok(buf)
}

/// Read up to `max` bytes starting at byte offset `offset`, using the
/// `AsyncSeek` capability of the underlying filesystem.
pub async fn read_at(
    fs: &dyn VirtualFileSystem,
    path: &VfsPath,
    offset: u64,
    max: usize,
) -> Result<Vec<u8>> {
    let mut seeker = fs.open_seek(path).await?;
    seeker
        .seek(SeekFrom::Start(offset))
        .await
        .map_err(|e| AIOSException::Generic(format!("seek failed: {e}")))?;
    let mut buf = vec![0u8; max];
    let mut got = 0usize;
    while got < max {
        let n = seeker
            .read(&mut buf[got..])
            .await
            .map_err(|e| AIOSException::Generic(format!("read failed: {e}")))?;
        if n == 0 {
            break;
        }
        got += n;
    }
    buf.truncate(got);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AclContext;
    use crate::vfs::{AiosVfs, HostVfs};

    #[tokio::test]
    async fn test_copy_recursive_between_schemes() {
        let dir = tempfile::tempdir().unwrap();
        let aios = AiosVfs::new(dir.path().join("sandbox")).unwrap();
        let host = HostVfs::new(
            Some(dir.path().join("host")),
            AclContext::with_tokens(&["vfs:host:read", "vfs:host:write"]),
        )
        .unwrap();

        let src = VfsPath::parse("AIOS:///sandbox").unwrap();
        aios.write_file(&src.join("a.txt"), b"alpha").await.unwrap();
        aios.create_dir(&src.join("sub")).await.unwrap();
        aios.write_file(&src.join("sub").join("b.txt"), b"beta")
            .await
            .unwrap();

        let dst = VfsPath::parse("HOST:///").unwrap();
        let progress = Progress::new();
        let stats = copy_recursive(&aios, &host, &src, &dst, &progress)
            .await
            .unwrap();
        assert_eq!(stats.files, 2);
        assert!(stats.dirs >= 1);

        let list = host.list_dir(&dst).await.unwrap();
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"sub"));
    }

    #[tokio::test]
    async fn test_move_within_scheme_uses_rename() {
        let dir = tempfile::tempdir().unwrap();
        let aios = AiosVfs::new(dir.path().to_path_buf()).unwrap();
        let src = VfsPath::parse("AIOS:///sandbox").unwrap();
        let file = src.join("m.txt");
        aios.write_file(&file, b"move me").await.unwrap();

        let progress = Progress::new();
        let stats = move_item(&aios, &aios, &file, &src.join("n.txt"), &progress)
            .await
            .unwrap();
        assert_eq!(stats.files, 1);
        assert!(aios.exists(&src.join("n.txt")).await.unwrap());
        assert!(!aios.exists(&file).await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let aios = AiosVfs::new(dir.path().to_path_buf()).unwrap();
        let root = VfsPath::parse("AIOS:///sandbox").unwrap();
        aios.write_file(&root.join("x.txt"), b"x").await.unwrap();
        aios.create_dir(&root.join("d")).await.unwrap();
        aios.write_file(&root.join("d").join("y.txt"), b"y")
            .await
            .unwrap();

        let progress = Progress::new();
        let stats = delete_item(&aios, &root, &progress).await.unwrap();
        assert_eq!(stats.files, 2);
        assert!(!aios.exists(&root).await.unwrap());
    }

    #[tokio::test]
    async fn test_copy_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let aios = AiosVfs::new(dir.path().to_path_buf()).unwrap();
        let src = VfsPath::parse("AIOS:///sandbox").unwrap();
        aios.write_file(&src.join("big.bin"), &[0u8; 1024])
            .await
            .unwrap();
        let progress = Progress::new();
        progress.cancel();
        let dst = VfsPath::parse("AIOS:///store").unwrap();
        assert!(copy_recursive(
            &aios,
            &aios,
            &src.join("big.bin"),
            &dst.join("big.bin"),
            &progress
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn test_read_head_and_read_at() {
        let dir = tempfile::tempdir().unwrap();
        let aios = AiosVfs::new(dir.path().to_path_buf()).unwrap();
        let file = VfsPath::parse("AIOS:///config/data.txt").unwrap();
        aios.write_file(&file, b"0123456789").await.unwrap();
        assert_eq!(read_head(&aios, &file, 4).await.unwrap(), b"0123");
        assert_eq!(read_at(&aios, &file, 5, 4).await.unwrap(), b"5678");
    }

    #[tokio::test]
    async fn test_total_bytes_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let aios = AiosVfs::new(dir.path().to_path_buf()).unwrap();
        let root = VfsPath::parse("AIOS:///sandbox").unwrap();
        aios.write_file(&root.join("a"), b"12345").await.unwrap();
        aios.create_dir(&root.join("d")).await.unwrap();
        aios.write_file(&root.join("d").join("b"), b"123")
            .await
            .unwrap();
        assert_eq!(total_bytes(&aios, &root).await.unwrap(), 8);
    }
}
