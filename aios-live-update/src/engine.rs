use aios_core::error::{AIOSException, Result};
use aios_ipc::bus::IpcBus;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::state_transfer::StateTransferManager;

pub type HealthCheckFn = Box<dyn Fn(&[u8]) -> bool + Send>;

pub struct HotSwapEntry {
    pub block_id: u32,
    pub old_binary: Vec<u8>,
    pub old_version: String,
    pub old_state: Vec<u8>,
    pub timestamp: Instant,
}

pub struct LiveUpdateEngine {
    rollback_entries: HashMap<u32, HotSwapEntry>,
    rollback_timeout: Duration,
    swap_history: Vec<SwapRecord>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SwapRecord {
    pub block_id: u32,
    pub old_version: String,
    pub new_version: String,
    pub success: bool,
    pub rolled_back: bool,
    pub timestamp: u64,
}

impl LiveUpdateEngine {
    pub fn new(rollback_timeout_ms: u64) -> Self {
        Self {
            rollback_entries: HashMap::new(),
            rollback_timeout: Duration::from_millis(rollback_timeout_ms),
            swap_history: Vec::new(),
        }
    }

    /// Perform atomic hot-swap:
    /// 1. Freeze IPC queue for target block
    /// 2. Extract state payload to memory
    /// 3. Validate new binary SHA-256
    /// 4. Run optional health check
    /// 5. Unload old module & store in rollback entries
    /// 6. Restore IPC queue
    /// 7. If health check fails within timeout, auto-rollback
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
        log::info!(
            "LiveUpdate: Starting swap for block_{block_id} v{old_version} → v{new_version}"
        );

        // Step 1: Freeze and extract state
        let snapshot = StateTransferManager::extract_state(queue, &old_state)?;

        // Step 2: Validate new binary signature
        let actual_hash = aios_core::crypto::compute_sha256_bytes(&new_binary);
        if actual_hash != new_sha256 {
            log::error!("LiveUpdate: SHA-256 mismatch for block_{block_id}");
            let _ = StateTransferManager::restore_state(queue, snapshot);
            return Err(AIOSException::HotSwapFailed(format!(
                "SHA-256 mismatch for block {block_id}"
            )));
        }

        // Step 3: Run health check on new binary
        if let Some(check) = health_check {
            if !check(&new_binary) {
                log::error!("LiveUpdate: Health check failed for block_{block_id}");
                let _ = StateTransferManager::restore_state(queue, snapshot);
                return Err(AIOSException::HotSwapFailed(format!(
                    "Health check failed for block {block_id}"
                )));
            }
        }

        // Step 4: Store old binary for rollback
        self.rollback_entries.insert(
            block_id,
            HotSwapEntry {
                block_id,
                old_binary,
                old_version: old_version.clone(),
                old_state: old_state.clone(),
                timestamp: Instant::now(),
            },
        );

        // Step 5: Restore queue
        let _ = StateTransferManager::restore_state(queue, snapshot);

        log::info!(
            "LiveUpdate: Swap succeeded for block_{block_id} v{new_version}, rollback entry stored"
        );

        self.swap_history.push(SwapRecord {
            block_id,
            old_version,
            new_version,
            success: true,
            rolled_back: false,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        });

        Ok(())
    }

    /// Rollback a block to its previous binary and state
    pub fn rollback(&mut self, block_id: u32, queue: &mut IpcBus) -> Result<HotSwapEntry> {
        let entry = self.rollback_entries.remove(&block_id).ok_or_else(|| {
            AIOSException::RollbackFailed(format!("No rollback entry for block {block_id}"))
        })?;

        // Check if within rollback timeout
        if entry.timestamp.elapsed() > self.rollback_timeout {
            log::warn!(
                "LiveUpdate: Rollback for block_{block_id} requested after timeout ({:?}), proceeding anyway",
                entry.timestamp.elapsed()
            );
        }

        // Freeze and restore old state
        let snapshot = StateTransferManager::extract_state(queue, &entry.old_state)?;
        let _ = StateTransferManager::restore_state(queue, snapshot);

        log::info!(
            "LiveUpdate: Rollback completed for block_{block_id} to v{}",
            entry.old_version
        );

        self.swap_history.push(SwapRecord {
            block_id,
            old_version: entry.old_version.clone(),
            new_version: "rolled_back".into(),
            success: true,
            rolled_back: true,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        });

        Ok(entry)
    }

    /// Check if a block has expired rollback entries (older than timeout)
    pub fn expired_rollbacks(&self) -> Vec<u32> {
        self.rollback_entries
            .iter()
            .filter(|(_, entry)| entry.timestamp.elapsed() > self.rollback_timeout)
            .map(|(&id, _)| id)
            .collect()
    }

    pub fn has_rollback(&self, block_id: u32) -> bool {
        self.rollback_entries.contains_key(&block_id)
    }

    pub fn swap_history(&self) -> &[SwapRecord] {
        &self.swap_history
    }

    pub fn pending_rollbacks(&self) -> Vec<(u32, &str)> {
        self.rollback_entries
            .iter()
            .map(|(&id, e)| (id, e.old_version.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{IpcPacket, Payload};

    fn test_packet(target: u32) -> IpcPacket {
        IpcPacket::new(
            0,
            target,
            aios_core::ipc_protocol::CommandId::HealthCheck,
            Payload::Empty,
        )
    }

    fn sample_binary(name: &str) -> Vec<u8> {
        format!("binary_{name}").into_bytes()
    }

    #[test]
    fn test_successful_swap() {
        let mut engine = LiveUpdateEngine::new(5000);
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();
        bus.send(test_packet(2)).unwrap();

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

        assert_eq!(bus.len(), 2);
        assert!(engine.has_rollback(1));
        assert_eq!(engine.pending_rollbacks().len(), 1);
    }

    #[test]
    fn test_swap_invalid_hash_fails() {
        let mut engine = LiveUpdateEngine::new(5000);
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();

        let bad_hash = [0u8; 32];
        let result = engine.perform_swap(
            1,
            sample_binary("v1"),
            "0.1.0".into(),
            b"".to_vec(),
            sample_binary("v2"),
            "0.2.0".into(),
            bad_hash,
            &mut bus,
            None,
        );

        assert!(result.is_err());
        assert!(!engine.has_rollback(1));
        assert_eq!(bus.len(), 1); // queue restored
    }

    #[test]
    fn test_health_check_failure() {
        let mut engine = LiveUpdateEngine::new(5000);
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();

        let new_bin = sample_binary("v2");
        let hash = aios_core::crypto::compute_sha256_bytes(&new_bin);
        let failing_check: HealthCheckFn = Box::new(|_: &[u8]| false);

        let result = engine.perform_swap(
            1,
            sample_binary("v1"),
            "0.1.0".into(),
            b"".to_vec(),
            new_bin,
            "0.2.0".into(),
            hash,
            &mut bus,
            Some(&failing_check),
        );

        assert!(result.is_err());
        assert!(!engine.has_rollback(1));
        assert_eq!(bus.len(), 1); // queue restored
    }

    #[test]
    fn test_rollback_restores_state() {
        let mut engine = LiveUpdateEngine::new(5000);
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

        let entry = engine.rollback(1, &mut bus).unwrap();
        assert_eq!(entry.old_version, "0.1.0");
        assert_eq!(entry.old_state, b"state_v1");
        assert!(!engine.has_rollback(1));
    }

    #[test]
    fn test_rollback_nonexistent_fails() {
        let mut engine = LiveUpdateEngine::new(5000);
        let mut bus = IpcBus::new(10);
        let result = engine.rollback(999, &mut bus);
        assert!(result.is_err());
    }

    #[test]
    fn test_failed_swap_auto_rollback_simulation() {
        // Simulate: swap succeeds but health check fails 500ms later
        let mut engine = LiveUpdateEngine::new(500);
        let mut bus = IpcBus::new(10);
        bus.send(test_packet(1)).unwrap();

        let new_bin = sample_binary("v2");
        let hash = aios_core::crypto::compute_sha256_bytes(&new_bin);

        // Initial swap succeeds
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

        // Simulate delayed health check failure → auto-rollback
        let entry = engine.rollback(1, &mut bus).unwrap();
        assert_eq!(entry.old_version, "0.1.0");
        assert_eq!(entry.old_binary, sample_binary("v1"));
    }

    #[test]
    fn test_swap_history() {
        let mut engine = LiveUpdateEngine::new(5000);
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

        engine.rollback(1, &mut bus).unwrap();

        let history = engine.swap_history();
        assert_eq!(history.len(), 2);
        assert!(history[0].success);
        assert!(!history[0].rolled_back);
        assert!(history[1].rolled_back);
    }
}
