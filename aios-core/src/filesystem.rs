use crate::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileSystemType {
    Local,
    Virtual,
    Overlay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size_bytes: u64,
    pub is_dir: bool,
    pub permissions: FilePermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePermissions {
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
}

impl Default for FilePermissions {
    fn default() -> Self {
        Self {
            readable: true,
            writable: false,
            executable: false,
        }
    }
}

impl FilePermissions {
    pub fn read_only() -> Self {
        Self {
            readable: true,
            writable: false,
            executable: false,
        }
    }

    pub fn read_write() -> Self {
        Self {
            readable: true,
            writable: true,
            executable: false,
        }
    }

    pub fn full() -> Self {
        Self {
            readable: true,
            writable: true,
            executable: true,
        }
    }
}

pub struct FileSystem {
    fs_type: FileSystemType,
    root: PathBuf,
    permissions: HashMap<String, FilePermissions>,
    virtual_files: HashMap<String, Vec<u8>>,
    read_only: bool,
}

impl FileSystem {
    pub fn local(root: PathBuf) -> Self {
        Self {
            fs_type: FileSystemType::Local,
            root,
            permissions: HashMap::new(),
            virtual_files: HashMap::new(),
            read_only: false,
        }
    }

    pub fn virtual_fs() -> Self {
        Self {
            fs_type: FileSystemType::Virtual,
            root: PathBuf::new(),
            permissions: HashMap::new(),
            virtual_files: HashMap::new(),
            read_only: false,
        }
    }

    pub fn overlay(underlying: PathBuf) -> Self {
        Self {
            fs_type: FileSystemType::Overlay,
            root: underlying,
            permissions: HashMap::new(),
            virtual_files: HashMap::new(),
            read_only: false,
        }
    }

    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn fs_type(&self) -> FileSystemType {
        self.fs_type
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn set_permission(&mut self, path: &str, perms: FilePermissions) {
        self.permissions.insert(path.to_string(), perms);
    }

    pub fn get_permission(&self, path: &str) -> FilePermissions {
        self.permissions
            .get(path)
            .copied()
            .unwrap_or_else(FilePermissions::default)
    }

    pub fn write_virtual(&mut self, path: &str, data: Vec<u8>) -> Result<()> {
        if self.read_only {
            return Err(AIOSException::PermissionDenied(format!(
                "Filesystem is read-only: cannot write '{}'",
                path
            )));
        }

        self.virtual_files.insert(path.to_string(), data);

        log::debug!(
            "FS: Wrote {} bytes to virtual path '{}'",
            self.virtual_files[path].len(),
            path
        );
        Ok(())
    }

    pub fn read_virtual(&self, path: &str) -> Result<&[u8]> {
        let perms = self.get_permission(path);
        if !perms.readable {
            return Err(AIOSException::PermissionDenied(format!(
                "Cannot read '{}' — not readable",
                path
            )));
        }

        self.virtual_files
            .get(path)
            .map(|d| d.as_slice())
            .ok_or_else(|| AIOSException::Generic(format!("Virtual file '{}' not found", path)))
    }

    pub fn delete_virtual(&mut self, path: &str) -> Result<()> {
        if self.read_only {
            return Err(AIOSException::PermissionDenied(format!(
                "Filesystem is read-only: cannot delete '{}'",
                path
            )));
        }

        self.virtual_files
            .remove(path)
            .ok_or_else(|| AIOSException::Generic(format!("Virtual file '{}' not found", path)))?;

        log::debug!("FS: Deleted virtual path '{}'", path);
        Ok(())
    }

    pub fn exists_virtual(&self, path: &str) -> bool {
        self.virtual_files.contains_key(path)
    }

    pub fn list_virtual(&self) -> Vec<&str> {
        self.virtual_files.keys().map(|s| s.as_str()).collect()
    }

    pub fn virtual_size(&self) -> u64 {
        self.virtual_files.values().map(|d| d.len() as u64).sum()
    }

    pub fn read_local(&self, path: &Path) -> Result<Vec<u8>> {
        if self.fs_type != FileSystemType::Local && self.fs_type != FileSystemType::Overlay {
            return Err(AIOSException::Generic(
                "Not a local/overlay filesystem".into(),
            ));
        }

        let full_path = self.root.join(path);
        std::fs::read(&full_path).map_err(|e| {
            AIOSException::Generic(format!("Read failed '{}': {e}", full_path.display()))
        })
    }

    pub fn write_local(&self, path: &Path, data: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(AIOSException::PermissionDenied(format!(
                "Filesystem is read-only: cannot write local '{}'",
                path.display()
            )));
        }

        if self.fs_type != FileSystemType::Local && self.fs_type != FileSystemType::Overlay {
            return Err(AIOSException::Generic(
                "Not a local/overlay filesystem".into(),
            ));
        }

        let full_path = self.root.join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AIOSException::Generic(format!("Create dir failed: {e}")))?;
        }
        std::fs::write(&full_path, data).map_err(|e| {
            AIOSException::Generic(format!("Write failed '{}': {e}", full_path.display()))
        })
    }

    pub fn list_local(&self, dir: &Path) -> Result<Vec<FileEntry>> {
        if self.fs_type != FileSystemType::Local && self.fs_type != FileSystemType::Overlay {
            return Err(AIOSException::Generic(
                "Not a local/overlay filesystem".into(),
            ));
        }

        let full_path = self.root.join(dir);
        let mut entries = Vec::new();

        if let Ok(read_dir) = std::fs::read_dir(&full_path) {
            for entry in read_dir.flatten() {
                let metadata = entry.metadata().ok();
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

                entries.push(FileEntry {
                    path: name,
                    size_bytes: size,
                    is_dir,
                    permissions: FilePermissions::read_only(),
                });
            }
        }

        Ok(entries)
    }

    pub fn virtual_file_count(&self) -> usize {
        self.virtual_files.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_fs_creation() {
        let fs = FileSystem::local(PathBuf::from("/tmp/test"));
        assert_eq!(fs.fs_type(), FileSystemType::Local);
        assert_eq!(fs.root(), Path::new("/tmp/test"));
        assert!(!fs.is_read_only());
    }

    #[test]
    fn test_virtual_fs_creation() {
        let fs = FileSystem::virtual_fs();
        assert_eq!(fs.fs_type(), FileSystemType::Virtual);
        assert!(fs.list_virtual().is_empty());
    }

    #[test]
    fn test_overlay_fs_creation() {
        let fs = FileSystem::overlay(PathBuf::from("/tmp/overlay"));
        assert_eq!(fs.fs_type(), FileSystemType::Overlay);
    }

    #[test]
    fn test_virtual_write_read() {
        let mut fs = FileSystem::virtual_fs();
        fs.write_virtual("test.bin", vec![1, 2, 3, 4]).unwrap();
        let data = fs.read_virtual("test.bin").unwrap();
        assert_eq!(data, &[1, 2, 3, 4]);
    }

    #[test]
    fn test_virtual_read_nonexistent() {
        let fs = FileSystem::virtual_fs();
        assert!(fs.read_virtual("nope").is_err());
    }

    #[test]
    fn test_virtual_delete() {
        let mut fs = FileSystem::virtual_fs();
        fs.write_virtual("a.bin", vec![1]).unwrap();
        assert!(fs.exists_virtual("a.bin"));
        fs.delete_virtual("a.bin").unwrap();
        assert!(!fs.exists_virtual("a.bin"));
    }

    #[test]
    fn test_virtual_delete_nonexistent() {
        let mut fs = FileSystem::virtual_fs();
        assert!(fs.delete_virtual("nope").is_err());
    }

    #[test]
    fn test_read_only_prevents_write() {
        let mut fs = FileSystem::virtual_fs();
        fs.set_read_only(true);
        assert!(fs.write_virtual("a.bin", vec![1]).is_err());
    }

    #[test]
    fn test_read_only_prevents_delete() {
        let mut fs = FileSystem::virtual_fs();
        fs.write_virtual("a.bin", vec![1]).unwrap();
        fs.set_read_only(true);
        assert!(fs.delete_virtual("a.bin").is_err());
    }

    #[test]
    fn test_read_only_prevents_local_write() {
        let mut fs = FileSystem::local(PathBuf::from("/tmp"));
        fs.set_read_only(true);
        assert!(fs.write_local(Path::new("test.txt"), b"hello").is_err());
    }

    #[test]
    fn test_permissions() {
        let mut fs = FileSystem::virtual_fs();
        fs.set_permission("/secret", FilePermissions::read_only());
        fs.write_virtual("/secret", vec![42]).unwrap();
        let data = fs.read_virtual("/secret").unwrap();
        assert_eq!(data, &[42]);
    }

    #[test]
    fn test_file_permissions_defaults() {
        let p = FilePermissions::default();
        assert!(p.readable);
        assert!(!p.writable);
        assert!(!p.executable);
    }

    #[test]
    fn test_file_permissions_read_write() {
        let p = FilePermissions::read_write();
        assert!(p.readable);
        assert!(p.writable);
    }

    #[test]
    fn test_file_permissions_full() {
        let p = FilePermissions::full();
        assert!(p.readable);
        assert!(p.writable);
        assert!(p.executable);
    }

    #[test]
    fn test_list_virtual() {
        let mut fs = FileSystem::virtual_fs();
        fs.write_virtual("a.bin", vec![1]).unwrap();
        fs.write_virtual("b.bin", vec![2]).unwrap();
        let list = fs.list_virtual();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_virtual_size() {
        let mut fs = FileSystem::virtual_fs();
        fs.write_virtual("a.bin", vec![0; 100]).unwrap();
        fs.write_virtual("b.bin", vec![0; 200]).unwrap();
        assert_eq!(fs.virtual_size(), 300);
    }

    #[test]
    fn test_file_entry_serialization() {
        let entry = FileEntry {
            path: "test.rs".into(),
            size_bytes: 1024,
            is_dir: false,
            permissions: FilePermissions::read_write(),
        };
        let bytes = bincode::serialize(&entry).unwrap();
        let restored: FileEntry = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.path, "test.rs");
        assert_eq!(restored.size_bytes, 1024);
    }

    #[test]
    fn test_file_permissions_serialization() {
        let p = FilePermissions::full();
        let bytes = bincode::serialize(&p).unwrap();
        let restored: FilePermissions = bincode::deserialize(&bytes).unwrap();
        assert!(restored.executable);
    }
}
