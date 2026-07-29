//! Copy-on-Write storage implementation

use aios_core::error::{AIOSException, Result};
use std::fs;
use std::path::PathBuf;

/// Copy-on-Write storage engine
pub struct CopyOnWriteStorage {
    base_dir: PathBuf,
    shadow_dir: PathBuf,
}

impl CopyOnWriteStorage {
    /// Create CoW storage
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        let shadow_dir = base_dir.join(".shadow");

        if !base_dir.exists() {
            fs::create_dir_all(&base_dir).map_err(|e| AIOSException::IPCError(e.to_string()))?;
        }

        if !shadow_dir.exists() {
            fs::create_dir_all(&shadow_dir).map_err(|e| AIOSException::IPCError(e.to_string()))?;
        }

        Ok(CopyOnWriteStorage {
            base_dir,
            shadow_dir,
        })
    }

    /// Write data atomically via CoW
    /// 1. Write to shadow region
    /// 2. Fsync
    /// 3. Atomic rename (becomes new primary)
    pub fn atomic_write(&self, file_name: &str, data: &[u8]) -> Result<()> {
        let primary_path = self.base_dir.join(file_name);
        let shadow_path = self.shadow_dir.join(file_name);

        // Step 1: Write to shadow
        fs::write(&shadow_path, data)
            .map_err(|e| AIOSException::IPCError(format!("Shadow write failed: {}", e)))?;

        // Step 2: Fsync (ensure data is on disk)
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&shadow_path)
            .map_err(|e| AIOSException::IPCError(e.to_string()))?;
        file.sync_all()
            .map_err(|e| AIOSException::IPCError(e.to_string()))?;
        drop(file);

        // Step 3: Atomic rename
        fs::rename(&shadow_path, &primary_path)
            .map_err(|e| AIOSException::IPCError(format!("Atomic rename failed: {}", e)))?;

        Ok(())
    }

    /// Read data
    pub fn read(&self, file_name: &str) -> Result<Vec<u8>> {
        let path = self.base_dir.join(file_name);
        fs::read(&path).map_err(|e| AIOSException::IPCError(e.to_string()))
    }

    /// Rollback to previous version (if exists)
    pub fn rollback(&self, file_name: &str) -> Result<bool> {
        let backup_path = self.base_dir.join(format!("{}.backup", file_name));
        let primary_path = self.base_dir.join(file_name);

        if !backup_path.exists() {
            return Ok(false);
        }

        // Keep current as backup for next rollback
        if primary_path.exists() {
            fs::copy(&primary_path, &backup_path)
                .map_err(|e| AIOSException::IPCError(e.to_string()))?;
        }

        // Restore from backup
        let data = fs::read(&backup_path).map_err(|e| AIOSException::IPCError(e.to_string()))?;

        self.atomic_write(file_name, &data)?;
        Ok(true)
    }

    /// Get file size
    pub fn file_size(&self, file_name: &str) -> Result<u64> {
        let path = self.base_dir.join(file_name);
        if !path.exists() {
            return Ok(0);
        }
        let metadata = fs::metadata(&path).map_err(|e| AIOSException::IPCError(e.to_string()))?;
        Ok(metadata.len())
    }

    /// Check if file exists
    pub fn exists(&self, file_name: &str) -> bool {
        self.base_dir.join(file_name).exists()
    }

    /// Delete file
    pub fn delete(&self, file_name: &str) -> Result<()> {
        let path = self.base_dir.join(file_name);
        if path.exists() {
            fs::remove_file(&path).map_err(|e| AIOSException::IPCError(e.to_string()))?;
        }
        Ok(())
    }

    /// Get total size of all files
    pub fn total_size(&self) -> Result<u64> {
        let mut total = 0u64;
        for entry in
            fs::read_dir(&self.base_dir).map_err(|e| AIOSException::IPCError(e.to_string()))?
        {
            let entry = entry.map_err(|e| AIOSException::IPCError(e.to_string()))?;
            let metadata = entry
                .metadata()
                .map_err(|e| AIOSException::IPCError(e.to_string()))?;
            if metadata.is_file() {
                total += metadata.len();
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_atomic_write_read() {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();

        let data = b"test data";
        storage.atomic_write("file1", data).unwrap();

        let read_data = storage.read("file1").unwrap();
        assert_eq!(read_data, data);
    }

    #[test]
    fn test_file_size() {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();

        let data = vec![0u8; 1000];
        storage.atomic_write("file1", &data).unwrap();

        let size = storage.file_size("file1").unwrap();
        assert_eq!(size, 1000);
    }

    #[test]
    fn test_exists() {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();

        assert!(!storage.exists("file1"));
        storage.atomic_write("file1", b"data").unwrap();
        assert!(storage.exists("file1"));
    }

    #[test]
    fn test_delete() {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();

        storage.atomic_write("file1", b"data").unwrap();
        assert!(storage.exists("file1"));

        storage.delete("file1").unwrap();
        assert!(!storage.exists("file1"));
    }

    #[test]
    fn test_total_size() {
        let temp_dir = TempDir::new().unwrap();
        let storage = CopyOnWriteStorage::new(temp_dir.path().to_path_buf()).unwrap();

        storage.atomic_write("file1", &vec![0u8; 100]).unwrap();
        storage.atomic_write("file2", &vec![0u8; 200]).unwrap();

        let total = storage.total_size().unwrap();
        assert_eq!(total, 300);
    }
}
