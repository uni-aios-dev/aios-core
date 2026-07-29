use aios_core::error::Result;
use aios_ipc::bus::IpcBus;
use aios_persistence::cow_storage::CopyOnWriteStorage;
use aios_persistence::recovery::RecoveryLog;
use std::path::PathBuf;

use crate::engine::{HealthCheckFn, LiveUpdateEngine, SwapRecord};

pub struct PersistedLiveUpdateEngine {
    engine: LiveUpdateEngine,
    storage: CopyOnWriteStorage,
    recovery_log: RecoveryLog,
}

impl PersistedLiveUpdateEngine {
    pub fn new(rollback_timeout_ms: u64, storage_dir: PathBuf) -> Result<Self> {
        let storage = CopyOnWriteStorage::new(storage_dir.join("live-update"))?;
        let recovery_log = RecoveryLog::new(storage_dir.join("recovery.log"), 10_000)?;

        Ok(Self {
            engine: LiveUpdateEngine::new(rollback_timeout_ms),
            storage,
            recovery_log,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn perform_swap(
        &mut self,
        block_id: u32,
        old_binary: Vec<u8>,
        old_version: String,
        old_state: Vec<u8>,
        new_binary: Vec<u8>,
        new_version: String,
        new_sha256: [u8; 32],
        queue: &mut IpcBus,
        health_check: Option<&HealthCheckFn>,
    ) -> Result<()> {
        self.recovery_log
            .log_entry("hotswap_start", &format!("block_{}", block_id))?;

        let state_file = format!("block_{}_state.bin", block_id);
        self.storage.atomic_write(&state_file, &old_state)?;

        let binary_file = format!("block_{}_old.bin", block_id);
        self.storage.atomic_write(&binary_file, &old_binary)?;

        let result = self.engine.perform_swap(
            block_id,
            old_binary,
            old_version.clone(),
            old_state,
            new_binary,
            new_version.clone(),
            new_sha256,
            queue,
            health_check,
        );

        match &result {
            Ok(()) => {
                self.recovery_log
                    .log_entry("hotswap_complete", &format!("block_{}", block_id))?;
            }
            Err(e) => {
                self.recovery_log
                    .log_entry("hotswap_failed", &format!("block_{}: {}", block_id, e))?;
            }
        }

        result
    }

    pub fn rollback(
        &mut self,
        block_id: u32,
        queue: &mut IpcBus,
    ) -> Result<aios_core::ipc_protocol::Payload> {
        self.recovery_log
            .log_entry("rollback_start", &format!("block_{}", block_id))?;

        let result = self.engine.rollback(block_id, queue);

        match &result {
            Ok(_entry) => {
                let state_file = format!("block_{}_state.bin", block_id);
                let _ = self.storage.delete(&state_file);
                let binary_file = format!("block_{}_old.bin", block_id);
                let _ = self.storage.delete(&binary_file);

                self.recovery_log
                    .log_entry("rollback_complete", &format!("block_{}", block_id))?;
            }
            Err(e) => {
                self.recovery_log
                    .log_entry("rollback_failed", &format!("block_{}: {}", block_id, e))?;
            }
        }

        result.map(|_| aios_core::ipc_protocol::Payload::Empty)
    }

    pub fn recover_from_crash(&mut self) -> Result<Vec<u32>> {
        let pending = self.recovery_log.get_pending_entries()?;
        let mut recovered_blocks = Vec::new();

        for entry in &pending {
            if entry.operation == "hotswap_start" {
                if let Some(block_id_str) = entry.target.strip_prefix("block_") {
                    if let Ok(block_id) = block_id_str.parse::<u32>() {
                        let state_file = format!("block_{}_state.bin", block_id);
                        if self.storage.exists(&state_file) {
                            recovered_blocks.push(block_id);
                            self.recovery_log.mark_completed(entry.id)?;
                            log::warn!("Recovery: found interrupted hotswap for block_{block_id}");
                        }
                    }
                }
            }
        }

        Ok(recovered_blocks)
    }

    pub fn swap_history(&self) -> &[SwapRecord] {
        self.engine.swap_history()
    }

    pub fn has_rollback(&self, block_id: u32) -> bool {
        self.engine.has_rollback(block_id)
    }

    pub fn storage_usage(&self) -> Result<u64> {
        self.storage.total_size()
    }

    pub fn engine(&self) -> &LiveUpdateEngine {
        &self.engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};

    fn test_packet(target: u32) -> IpcPacket {
        IpcPacket::new(0, target, CommandId::HealthCheck, Payload::Empty)
    }

    fn sample_binary(name: &str) -> Vec<u8> {
        format!("binary_{name}").into_bytes()
    }

    #[test]
    fn test_persisted_swap() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut engine =
            PersistedLiveUpdateEngine::new(5000, temp_dir.path().to_path_buf()).unwrap();
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();

        let new_bin = sample_binary("v2");
        let hash = aios_core::crypto::compute_sha256_bytes(&new_bin);

        engine
            .perform_swap(
                1,
                sample_binary("v1"),
                "0.1.0".into(),
                b"state_v1".to_vec(),
                new_bin,
                "0.2.0".into(),
                hash,
                &mut bus,
                None,
            )
            .unwrap();

        assert!(engine.has_rollback(1));
        assert!(engine.storage_usage().unwrap() > 0);
    }

    #[test]
    fn test_persisted_rollback() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut engine =
            PersistedLiveUpdateEngine::new(5000, temp_dir.path().to_path_buf()).unwrap();
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();

        let new_bin = sample_binary("v2");
        let hash = aios_core::crypto::compute_sha256_bytes(&new_bin);

        engine
            .perform_swap(
                1,
                sample_binary("v1"),
                "0.1.0".into(),
                b"state_v1".to_vec(),
                new_bin,
                "0.2.0".into(),
                hash,
                &mut bus,
                None,
            )
            .unwrap();

        engine.rollback(1, &mut bus).unwrap();
        assert!(!engine.has_rollback(1));
    }

    #[test]
    fn test_recover_from_crash() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut engine =
            PersistedLiveUpdateEngine::new(5000, temp_dir.path().to_path_buf()).unwrap();

        engine
            .recovery_log
            .log_entry("hotswap_start", "block_42")
            .unwrap();

        engine
            .storage
            .atomic_write("block_42_state.bin", b"old_state")
            .unwrap();

        let recovered = engine.recover_from_crash().unwrap();
        assert_eq!(recovered, vec![42]);
    }

    #[test]
    fn test_swap_history_tracking() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut engine =
            PersistedLiveUpdateEngine::new(5000, temp_dir.path().to_path_buf()).unwrap();
        let mut bus = IpcBus::new(10);

        let new_bin = sample_binary("v2");
        let hash = aios_core::crypto::compute_sha256_bytes(&new_bin);

        engine
            .perform_swap(
                1,
                sample_binary("v1"),
                "0.1.0".into(),
                b"".to_vec(),
                new_bin,
                "0.2.0".into(),
                hash,
                &mut bus,
                None,
            )
            .unwrap();

        assert_eq!(engine.swap_history().len(), 1);
        assert!(engine.swap_history()[0].success);
    }
}
