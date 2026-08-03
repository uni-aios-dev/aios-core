use crate::config::NetworkConfig;
use crate::store::NetworkConfigStore;
use aios_core::block::{BlockId, BlockState, StatefulBlock};
use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};

/// Kernel block exposing the network configuration over the IPC bus.
///
/// Supported custom commands:
/// - `net_get` → `Payload::Text` with the whole config as JSON
/// - `net_set <json>` → applies a partial JSON update and persists it
/// - `net_reset` → restores the factory-default configuration
/// - `net_persist` → forces a save to disk
pub struct NetSettingsBlock {
    id: BlockId,
    config: NetworkConfig,
    store: NetworkConfigStore,
    state: BlockState,
}

impl NetSettingsBlock {
    /// Create the block; when a config file exists at `store_path` it is
    /// loaded, otherwise `default` is used (and saved on first change).
    pub fn new(
        id: BlockId,
        default: NetworkConfig,
        store_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        let store = NetworkConfigStore::new(store_path);
        let config = store.load_or(default).unwrap_or_else(|e| {
            log::warn!("NetSettingsBlock: failed to load config: {e}; using defaults");
            NetworkConfig::default()
        });
        Self {
            id,
            config,
            store,
            state: BlockState::Active,
        }
    }

    /// Create the block using the default on-disk location.
    pub fn with_default_store(id: BlockId, default: NetworkConfig) -> Self {
        Self::new(id, default, NetworkConfigStore::default_path())
    }

    /// Current configuration snapshot.
    pub fn config(&self) -> &NetworkConfig {
        &self.config
    }

    /// Apply a partial JSON update and persist it to disk.
    pub fn apply(&mut self, updates: &serde_json::Value) -> Result<()> {
        self.config
            .apply_updates(updates)
            .map_err(AIOSException::ConfigurationError)?;
        self.store
            .save(&self.config)
            .map_err(AIOSException::ConfigurationError)
    }

    /// Restore factory defaults and persist them.
    pub fn reset(&mut self) -> Result<()> {
        self.config = NetworkConfig::default();
        self.store
            .save(&self.config)
            .map_err(AIOSException::ConfigurationError)
    }

    fn handle_custom(&mut self, command: &str, data: &[u8]) -> Result<IpcPacket> {
        let response_payload = match command {
            "net_get" => Payload::Text(self.config.to_json()),
            "net_set" => {
                let updates: serde_json::Value = serde_json::from_slice(data)
                    .map_err(|e| AIOSException::InvalidPayload(format!("Bad JSON: {e}")))?;
                self.apply(&updates)?;
                Payload::Text(self.config.to_json())
            }
            "net_reset" => {
                self.reset()?;
                Payload::Text(self.config.to_json())
            }
            "net_persist" => {
                self.store
                    .save(&self.config)
                    .map_err(AIOSException::ConfigurationError)?;
                Payload::Text("persisted".into())
            }
            other => {
                return Err(AIOSException::InvalidPayload(format!(
                    "Unknown net command: {other}"
                )));
            }
        };
        Ok(IpcPacket::response_ok(self.id.0, 0, 1, response_payload))
    }
}

impl StatefulBlock for NetSettingsBlock {
    fn id(&self) -> BlockId {
        self.id
    }

    fn name(&self) -> &str {
        "net_settings"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn state(&self) -> BlockState {
        self.state
    }

    fn handle_message(&mut self, packet: &IpcPacket) -> Result<Option<IpcPacket>> {
        match (&packet.payload, packet.header.command_id) {
            (_, id) if id == CommandId::HealthCheck as u16 => Ok(Some(IpcPacket::response_ok(
                self.id.0,
                packet.header.source_block,
                packet.header.packet_id,
                Payload::Text("healthy".into()),
            ))),
            (Payload::Custom(command, data), id) if id == CommandId::Custom as u16 => {
                let response = self.handle_custom(command, data)?;
                Ok(Some(response))
            }
            (Payload::ExtractState, _) => {
                let state = self.extract_state()?;
                Ok(Some(IpcPacket::response_ok(
                    self.id.0,
                    packet.header.source_block,
                    packet.header.packet_id,
                    Payload::Binary(state),
                )))
            }
            (Payload::RestoreState(state), _) => {
                self.restore_state(state)?;
                Ok(Some(IpcPacket::response_ok(
                    self.id.0,
                    packet.header.source_block,
                    packet.header.packet_id,
                    Payload::Text("restored".into()),
                )))
            }
            _ => Err(AIOSException::InvalidPayload(format!(
                "Unsupported message for net_settings: {:?}",
                packet.payload
            ))),
        }
    }

    fn extract_state(&self) -> Result<Vec<u8>> {
        bincode::serialize(&self.config)
            .map_err(|e| AIOSException::StateExtractionFailed(e.to_string()))
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<()> {
        let config: NetworkConfig = bincode::deserialize(state)
            .map_err(|e| AIOSException::StateRestoreFailed(e.to_string()))?;
        self.config = config;
        self.store
            .save(&self.config)
            .map_err(AIOSException::StateRestoreFailed)
    }

    fn health_check(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::Payload;

    fn test_block(dir: &tempfile::TempDir) -> NetSettingsBlock {
        NetSettingsBlock::new(
            BlockId::new(7),
            NetworkConfig::default(),
            dir.path().join("net.json"),
        )
    }

    fn custom_block_packet(target: u32, command: &str, data: Vec<u8>) -> IpcPacket {
        IpcPacket::new(
            0,
            target,
            CommandId::Custom,
            Payload::Custom(command.into(), data),
        )
    }

    fn response_text(response: &IpcPacket) -> String {
        match &response.payload {
            Payload::Text(t) => t.clone(),
            other => panic!("expected text response, got {other:?}"),
        }
    }

    #[test]
    fn test_block_identity() {
        let dir = tempfile::tempdir().unwrap();
        let block = test_block(&dir);
        assert_eq!(block.id(), BlockId::new(7));
        assert_eq!(block.name(), "net_settings");
        assert_eq!(block.version(), "1.0.0");
        assert_eq!(block.state(), BlockState::Active);
    }

    #[test]
    fn test_get_returns_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut block = test_block(&dir);
        let packet = custom_block_packet(7, "net_get", Vec::new());
        let response = block.handle_message(&packet).unwrap().unwrap();
        let json = response_text(&response);
        let config = NetworkConfig::from_json(&json).unwrap();
        assert_eq!(config.hostname, "aios-host");
    }

    #[test]
    fn test_set_applies_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut block = test_block(&dir);
        let updates = serde_json::json!({ "hostname": "set-host", "listen_port": 9090 });
        let packet = custom_block_packet(7, "net_set", updates.to_string().into_bytes());
        block.handle_message(&packet).unwrap().unwrap();

        let reloaded = block.store.load().unwrap().unwrap();
        assert_eq!(reloaded.hostname, "set-host");
        assert_eq!(reloaded.listen_port, 9090);
    }

    #[test]
    fn test_set_rejects_invalid_port() {
        let dir = tempfile::tempdir().unwrap();
        let mut block = test_block(&dir);
        let updates = serde_json::json!({ "listen_port": 0 });
        let packet = custom_block_packet(7, "net_set", updates.to_string().into_bytes());
        assert!(block.handle_message(&packet).is_err());
    }

    #[test]
    fn test_reset_restores_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let mut block = test_block(&dir);
        block
            .apply(&serde_json::json!({ "hostname": "changed" }))
            .unwrap();
        let packet = custom_block_packet(7, "net_reset", Vec::new());
        block.handle_message(&packet).unwrap().unwrap();
        assert_eq!(block.config().hostname, "aios-host");
    }

    #[test]
    fn test_unknown_command_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut block = test_block(&dir);
        let packet = custom_block_packet(7, "net_do_the_thing", Vec::new());
        assert!(block.handle_message(&packet).is_err());
    }

    #[test]
    fn test_health_check_message() {
        let dir = tempfile::tempdir().unwrap();
        let mut block = test_block(&dir);
        let packet = IpcPacket::new(0, 7, CommandId::HealthCheck, Payload::Empty);
        let response = block.handle_message(&packet).unwrap().unwrap();
        assert_eq!(response_text(&response), "healthy");
    }

    #[test]
    fn test_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut block = test_block(&dir);
        block
            .apply(&serde_json::json!({ "hostname": "stateful" }))
            .unwrap();
        let state = block.extract_state().unwrap();
        let mut restored = test_block(&dir);
        restored.restore_state(&state).unwrap();
        assert_eq!(restored.config().hostname, "stateful");
    }
}
