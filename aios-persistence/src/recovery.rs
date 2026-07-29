//! Recovery log for crash resilience during state transfer

use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

/// Recovery log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryEntry {
    /// Entry ID
    pub id: u64,
    /// Timestamp (Unix milliseconds)
    pub timestamp_ms: u64,
    /// Operation type
    pub operation: String,
    /// Block ID or target
    pub target: String,
    /// Status (pending, completed, failed)
    pub status: String,
    /// Additional data
    pub metadata: Option<Vec<u8>>,
}

/// Recovery log for tracking in-flight operations
pub struct RecoveryLog {
    log_path: PathBuf,
    max_entries: usize,
    next_id: u64,
}

impl RecoveryLog {
    /// Create recovery log
    pub fn new(log_path: PathBuf, max_entries: usize) -> Result<Self> {
        // Create parent directory if needed
        if let Some(parent) = log_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| AIOSException::IPCError(e.to_string()))?;
            }
        }

        Ok(RecoveryLog {
            log_path,
            max_entries,
            next_id: 0,
        })
    }

    /// Log an entry
    pub fn log_entry(&mut self, op: &str, target: &str) -> Result<u64> {
        let entry = RecoveryEntry {
            id: self.next_id,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            operation: op.to_string(),
            target: target.to_string(),
            status: "pending".to_string(),
            metadata: None,
        };

        let data = bincode::serialize(&entry)
            .map_err(|e| AIOSException::SerializationError(e.to_string()))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|e| AIOSException::IPCError(e.to_string()))?;

        file.write_all(&data)
            .map_err(|e| AIOSException::IPCError(e.to_string()))?;
        file.write_all(b"\n")
            .map_err(|e| AIOSException::IPCError(e.to_string()))?;

        let id = self.next_id;
        self.next_id += 1;

        // Cleanup old entries if needed
        if self.next_id > self.max_entries as u64 {
            self.cleanup()?;
        }

        Ok(id)
    }

    /// Mark entry as completed
    pub fn mark_completed(&self, entry_id: u64) -> Result<()> {
        // For simplicity, we'll just append completion marker
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|e| AIOSException::IPCError(e.to_string()))?;

        let marker = format!("COMPLETED:{}\n", entry_id);
        file.write_all(marker.as_bytes())
            .map_err(|e| AIOSException::IPCError(e.to_string()))?;

        Ok(())
    }

    /// Get recovery entries (incomplete operations)
    pub fn get_pending_entries(&self) -> Result<Vec<RecoveryEntry>> {
        if !self.log_path.exists() {
            return Ok(Vec::new());
        }

        let content =
            fs::read(&self.log_path).map_err(|e| AIOSException::IPCError(e.to_string()))?;

        let mut completed_ids = Vec::new();
        let mut raw_entries = Vec::new();
        let mut offset = 0;

        while offset < content.len() {
            let remaining = &content[offset..];
            if let Some(newline_pos) = remaining.iter().position(|&b| b == b'\n') {
                let line_bytes = &remaining[..newline_pos];

                if let Some(id_bytes) = line_bytes.strip_prefix(b"COMPLETED:") {
                    if let Ok(s) = std::str::from_utf8(id_bytes) {
                        if let Ok(id) = s.trim().parse::<u64>() {
                            completed_ids.push(id);
                        }
                    }
                } else if let Ok(entry) = bincode::deserialize::<RecoveryEntry>(line_bytes) {
                    raw_entries.push(entry);
                }

                offset += newline_pos + 1;
            } else {
                break;
            }
        }

        let entries = raw_entries
            .into_iter()
            .filter(|e| e.status == "pending" && !completed_ids.contains(&e.id))
            .collect();

        Ok(entries)
    }

    /// Clear recovery log
    pub fn clear(&self) -> Result<()> {
        if self.log_path.exists() {
            fs::remove_file(&self.log_path).map_err(|e| AIOSException::IPCError(e.to_string()))?;
        }
        Ok(())
    }

    /// Cleanup old entries (FIFO)
    fn cleanup(&mut self) -> Result<()> {
        // Read all entries
        let entries = self.get_pending_entries()?;

        // Keep only the last max_entries
        if entries.len() > self.max_entries {
            // Recreate log with only recent entries
            self.clear()?;
            self.next_id = 0;

            for entry in entries.iter().skip(entries.len() - self.max_entries) {
                let data = bincode::serialize(entry)
                    .map_err(|e| AIOSException::SerializationError(e.to_string()))?;

                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.log_path)
                    .map_err(|e| AIOSException::IPCError(e.to_string()))?;

                file.write_all(&data)
                    .map_err(|e| AIOSException::IPCError(e.to_string()))?;
                file.write_all(b"\n")
                    .map_err(|e| AIOSException::IPCError(e.to_string()))?;

                self.next_id += 1;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_recovery_log_entry() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut log = RecoveryLog::new(temp_file.path().to_path_buf(), 100).unwrap();

        let id = log.log_entry("hotswap", "block_1").unwrap();
        assert_eq!(id, 0);

        let id2 = log.log_entry("rollback", "block_1").unwrap();
        assert_eq!(id2, 1);
    }

    #[test]
    fn test_recovery_log_pending() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut log = RecoveryLog::new(temp_file.path().to_path_buf(), 100).unwrap();

        log.log_entry("hotswap", "block_1").unwrap();
        log.mark_completed(0).unwrap();

        let pending = log.get_pending_entries().unwrap();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn test_recovery_log_clear() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut log = RecoveryLog::new(temp_file.path().to_path_buf(), 100).unwrap();

        log.log_entry("test", "target").unwrap();
        log.clear().unwrap();

        // File may or may not exist after clear on Windows
    }
}
