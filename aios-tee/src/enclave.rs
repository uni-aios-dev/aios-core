//! TEE Enclave Lifecycle Management
//!
//! Provides enclave creation, initialization, execution, and teardown operations
//! with state preservation and error recovery.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Enclave execution state
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EnclaveState {
    Created,
    Initialized,
    Running,
    Suspended,
    Exited,
    Failed,
}

/// Enclave configuration parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnclaveConfig {
    memory_size: u64,
    max_threads: u32,
    debug_mode: bool,
    tcs_num: u32,
}

impl EnclaveConfig {
    /// Create new enclave configuration
    pub fn new(memory_size: u64, max_threads: u32) -> Self {
        Self {
            memory_size,
            max_threads,
            debug_mode: false,
            tcs_num: max_threads,
        }
    }

    /// Set debug mode
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug_mode = debug;
        self
    }

    /// Get memory size
    pub fn memory_size(&self) -> u64 {
        self.memory_size
    }

    /// Get max threads
    pub fn max_threads(&self) -> u32 {
        self.max_threads
    }

    /// Is debug mode enabled
    pub fn is_debug(&self) -> bool {
        self.debug_mode
    }
}

/// TEE Enclave instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enclave {
    id: u32,
    state: EnclaveState,
    config: EnclaveConfig,
    created_at: u64,
    initialized_at: u64,
    attributes: u64,
    measurement: Vec<u8>,
    thread_count: u32,
    custom_data: HashMap<String, Vec<u8>>,
}

impl Enclave {
    /// Create a new enclave
    pub fn new(id: u32, config: EnclaveConfig) -> Self {
        Self {
            id,
            state: EnclaveState::Created,
            config,
            created_at: 0,
            initialized_at: 0,
            attributes: 0,
            measurement: Vec::new(),
            thread_count: 0,
            custom_data: HashMap::new(),
        }
    }

    /// Get enclave ID
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Get current state
    pub fn state(&self) -> EnclaveState {
        self.state
    }

    /// Get configuration
    pub fn config(&self) -> &EnclaveConfig {
        &self.config
    }

    /// Initialize enclave
    pub fn initialize(&mut self, timestamp: u64, measurement: Vec<u8>) {
        self.state = EnclaveState::Initialized;
        self.initialized_at = timestamp;
        self.measurement = measurement;
    }

    /// Start enclave execution
    pub fn start(&mut self) {
        if self.state == EnclaveState::Initialized {
            self.state = EnclaveState::Running;
        }
    }

    /// Suspend enclave execution
    pub fn suspend(&mut self) {
        if self.state == EnclaveState::Running {
            self.state = EnclaveState::Suspended;
        }
    }

    /// Resume enclave execution
    pub fn resume(&mut self) {
        if self.state == EnclaveState::Suspended {
            self.state = EnclaveState::Running;
        }
    }

    /// Exit enclave
    pub fn exit(&mut self) {
        self.state = EnclaveState::Exited;
    }

    /// Mark enclave as failed
    pub fn fail(&mut self) {
        self.state = EnclaveState::Failed;
    }

    /// Add running thread
    pub fn add_thread(&mut self) -> bool {
        if self.thread_count < self.config.max_threads {
            self.thread_count += 1;
            true
        } else {
            false
        }
    }

    /// Remove running thread
    pub fn remove_thread(&mut self) {
        if self.thread_count > 0 {
            self.thread_count -= 1;
        }
    }

    /// Get thread count
    pub fn thread_count(&self) -> u32 {
        self.thread_count
    }

    /// Set custom data
    pub fn set_data(&mut self, key: String, value: Vec<u8>) {
        self.custom_data.insert(key, value);
    }

    /// Get custom data
    pub fn get_data(&self, key: &str) -> Option<&Vec<u8>> {
        self.custom_data.get(key)
    }

    /// Get measurement
    pub fn measurement(&self) -> &[u8] {
        &self.measurement
    }

    /// Get initialization time
    pub fn initialized_at(&self) -> u64 {
        self.initialized_at
    }

    /// Serialize to binary format
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }

    /// Deserialize from binary format
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        bincode::deserialize(data).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enclave_config_creation() {
        let config = EnclaveConfig::new(4096, 4);
        assert_eq!(config.memory_size(), 4096);
        assert_eq!(config.max_threads(), 4);
        assert!(!config.is_debug());
    }

    #[test]
    fn test_enclave_config_with_debug() {
        let config = EnclaveConfig::new(4096, 4).with_debug(true);
        assert!(config.is_debug());
    }

    #[test]
    fn test_enclave_creation() {
        let config = EnclaveConfig::new(4096, 4);
        let enclave = Enclave::new(1, config);

        assert_eq!(enclave.id(), 1);
        assert_eq!(enclave.state(), EnclaveState::Created);
        assert_eq!(enclave.thread_count(), 0);
    }

    #[test]
    fn test_enclave_initialization() {
        let config = EnclaveConfig::new(4096, 4);
        let mut enclave = Enclave::new(1, config);

        enclave.initialize(1000, vec![1; 32]);
        assert_eq!(enclave.state(), EnclaveState::Initialized);
        assert_eq!(enclave.initialized_at(), 1000);
        assert_eq!(enclave.measurement().len(), 32);
    }

    #[test]
    fn test_enclave_lifecycle() {
        let config = EnclaveConfig::new(4096, 4);
        let mut enclave = Enclave::new(1, config);

        enclave.initialize(1000, vec![1; 32]);
        assert_eq!(enclave.state(), EnclaveState::Initialized);

        enclave.start();
        assert_eq!(enclave.state(), EnclaveState::Running);

        enclave.suspend();
        assert_eq!(enclave.state(), EnclaveState::Suspended);

        enclave.resume();
        assert_eq!(enclave.state(), EnclaveState::Running);

        enclave.exit();
        assert_eq!(enclave.state(), EnclaveState::Exited);
    }

    #[test]
    fn test_enclave_thread_management() {
        let config = EnclaveConfig::new(4096, 2);
        let mut enclave = Enclave::new(1, config);

        assert!(enclave.add_thread());
        assert_eq!(enclave.thread_count(), 1);

        assert!(enclave.add_thread());
        assert_eq!(enclave.thread_count(), 2);

        assert!(!enclave.add_thread());
        assert_eq!(enclave.thread_count(), 2);

        enclave.remove_thread();
        assert_eq!(enclave.thread_count(), 1);
    }

    #[test]
    fn test_enclave_custom_data() {
        let config = EnclaveConfig::new(4096, 4);
        let mut enclave = Enclave::new(1, config);

        enclave.set_data("key1".to_string(), vec![1, 2, 3]);
        enclave.set_data("key2".to_string(), vec![4, 5, 6]);

        assert_eq!(enclave.get_data("key1"), Some(&vec![1, 2, 3]));
        assert_eq!(enclave.get_data("key2"), Some(&vec![4, 5, 6]));
        assert_eq!(enclave.get_data("key3"), None);
    }

    #[test]
    fn test_enclave_failure() {
        let config = EnclaveConfig::new(4096, 4);
        let mut enclave = Enclave::new(1, config);

        enclave.initialize(1000, vec![1; 32]);
        enclave.start();
        enclave.fail();

        assert_eq!(enclave.state(), EnclaveState::Failed);
    }

    #[test]
    fn test_enclave_serialization() {
        let config = EnclaveConfig::new(4096, 4);
        let mut original = Enclave::new(1, config);

        original.initialize(1000, vec![1; 32]);
        original.start();
        original.set_data("key".to_string(), vec![1, 2, 3]);

        let bytes = original.to_bytes();
        let recovered = Enclave::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.id(), original.id());
        assert_eq!(recovered.state(), original.state());
        assert_eq!(recovered.get_data("key"), Some(&vec![1, 2, 3]));
    }

    #[test]
    fn test_enclave_state_transitions() {
        let config = EnclaveConfig::new(4096, 4);
        let mut enclave = Enclave::new(1, config);

        enclave.initialize(1000, vec![1; 32]);
        enclave.start();

        enclave.suspend();
        assert_eq!(enclave.state(), EnclaveState::Suspended);

        enclave.resume();
        assert_eq!(enclave.state(), EnclaveState::Running);

        enclave.suspend();
        assert_eq!(enclave.state(), EnclaveState::Suspended);
    }

    #[test]
    fn test_enclave_thread_count_boundary() {
        let config = EnclaveConfig::new(4096, 1);
        let mut enclave = Enclave::new(1, config);

        assert!(enclave.add_thread());
        assert!(!enclave.add_thread());
        assert!(!enclave.add_thread());
    }
}
