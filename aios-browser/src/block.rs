use crate::engine::BrowserEngine;
use crate::types::{BrowserConfig, BrowserError, Page};
use aios_core::block::{BlockId, BlockState, StatefulBlock};
use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use std::future::Future;

/// First-class AIOS block that wraps [`BrowserEngine`] and exposes browsing
/// through the kernel IPC channel.
///
/// The block drives the async [`BrowserEngine::navigate`] on a dedicated
/// on-demand Tokio runtime, so the synchronous [`StatefulBlock::handle_message`]
/// never blocks the caller's runtime and never panics on nested runtimes.
pub struct BrowserBlock {
    id: BlockId,
    engine: BrowserEngine,
    state: BlockState,
}

impl BrowserBlock {
    /// Creates a browser block with the given block id and browsing config.
    pub fn new(id: BlockId, config: BrowserConfig) -> Self {
        let engine = BrowserEngine::new(config);
        Self {
            id,
            engine,
            state: BlockState::Active,
        }
    }

    /// Returns a reference to the underlying browser engine.
    pub fn engine(&self) -> &BrowserEngine {
        &self.engine
    }

    /// Runs an async future to completion from a synchronous context.
    ///
    /// If the current thread already runs inside a Tokio runtime, the future
    /// is driven on a dedicated OS thread (runtime created and dropped there)
    /// to avoid the nested-runtime panic.
    fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        let runtime = Self::new_runtime();
                        runtime.block_on(future)
                    })
                    .join()
                    .expect("browser block async task panicked")
            })
        } else {
            let runtime = Self::new_runtime();
            runtime.block_on(future)
        }
    }

    /// Builds a dedicated current-thread Tokio runtime for a single navigation.
    fn new_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build browser block runtime")
    }

    /// Fetches and parses a page through the underlying engine.
    fn browse(&self, url: &str) -> std::result::Result<Page, BrowserError> {
        self.block_on(self.engine.navigate(url))
    }

    /// Handles a custom IPC command, returning an error for unknown commands.
    fn handle_custom(
        &self,
        packet: &IpcPacket,
        cmd_name: &str,
        data: &[u8],
    ) -> Result<Option<IpcPacket>> {
        match cmd_name {
            "browse" => {
                let url = String::from_utf8_lossy(data).to_string();
                if url.trim().is_empty() {
                    return Err(AIOSException::InvalidPayload("empty URL".into()));
                }
                match self.browse(&url) {
                    Ok(page) => {
                        let bytes = bincode::serialize(&page)
                            .map_err(|e| AIOSException::SerializationError(e.to_string()))?;
                        Ok(Some(IpcPacket::response_ok(
                            self.id.0,
                            packet.header.source_block,
                            packet.header.packet_id,
                            Payload::Binary(bytes),
                        )))
                    }
                    Err(e) => Err(AIOSException::Generic(e.to_string())),
                }
            }
            "open_native" => {
                let url = String::from_utf8_lossy(data).to_string();
                if url.trim().is_empty() {
                    return Err(AIOSException::InvalidPayload("empty URL".into()));
                }
                match open::that(&url) {
                    Ok(()) => Ok(Some(IpcPacket::response_ok(
                        self.id.0,
                        packet.header.source_block,
                        packet.header.packet_id,
                        Payload::Text(format!("opened in native browser: {url}")),
                    ))),
                    Err(e) => Err(AIOSException::Generic(format!(
                        "failed to open native browser: {e}"
                    ))),
                }
            }
            "browser_status" => {
                let status = serde_json::json!({
                    "name": "browser",
                    "version": self.version(),
                    "state": format!("{:?}", self.state),
                    "user_agent": self.engine.config().user_agent,
                    "timeout_secs": self.engine.config().timeout_secs,
                    "sandbox_enabled": self.engine.config().sandbox_enabled,
                });
                Ok(Some(IpcPacket::response_ok(
                    self.id.0,
                    packet.header.source_block,
                    packet.header.packet_id,
                    Payload::Text(status.to_string()),
                )))
            }
            _ => Err(AIOSException::IPCError(format!(
                "browser unknown custom command '{cmd_name}'"
            ))),
        }
    }
}

impl StatefulBlock for BrowserBlock {
    fn id(&self) -> BlockId {
        self.id
    }

    fn name(&self) -> &str {
        "browser"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn state(&self) -> BlockState {
        self.state
    }

    fn handle_message(&mut self, packet: &IpcPacket) -> Result<Option<IpcPacket>> {
        match packet.header.command_id {
            cmd if cmd == CommandId::HealthCheck as u16 => Ok(Some(IpcPacket::response_ok(
                self.id.0,
                packet.header.source_block,
                packet.header.packet_id,
                Payload::Binary(b"browser-ok".to_vec()),
            ))),
            cmd if cmd == CommandId::Custom as u16 => {
                let (cmd_name, data) = match &packet.payload {
                    Payload::Custom(name, bytes) => (name.as_str(), bytes.as_slice()),
                    Payload::Text(text) => ("browse", text.as_bytes()),
                    _ => {
                        return Err(AIOSException::InvalidPayload(
                            "browser block expects Custom or Text payload".into(),
                        ))
                    }
                };
                self.handle_custom(packet, cmd_name, data)
            }
            _ => Err(AIOSException::IPCError(format!(
                "browser does not handle command 0x{:04X}",
                packet.header.command_id
            ))),
        }
    }

    fn health_check(&self) -> bool {
        self.engine.config().timeout_secs > 0
    }

    fn extract_state(&self) -> Result<Vec<u8>> {
        let state = (self.engine.config().clone(), self.state);
        bincode::serialize(&state).map_err(|e| AIOSException::StateExtractionFailed(e.to_string()))
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<()> {
        let (_, restored): (BrowserConfig, BlockState) = bincode::deserialize(state)
            .map_err(|e| AIOSException::StateRestoreFailed(e.to_string()))?;
        self.state = restored;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{CommandId, Header};

    fn test_packet(target: u32, command: CommandId, payload: Payload) -> IpcPacket {
        IpcPacket {
            header: Header {
                packet_id: 1,
                source_block: 0,
                target_block: target,
                command_id: command as u16,
                priority: 2,
                payload_len: payload.to_bytes().len() as u32,
                checksum: [0u8; 32],
            },
            payload,
        }
    }

    #[test]
    fn test_browser_block_creation() {
        let block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        assert_eq!(block.name(), "browser");
        assert_eq!(block.id(), BlockId::new(4));
        assert_eq!(block.state(), BlockState::Active);
    }

    #[test]
    fn test_browser_block_health_check() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        assert!(block.health_check());
        assert!(block
            .handle_message(&test_packet(4, CommandId::HealthCheck, Payload::Empty))
            .is_ok());
    }

    #[test]
    fn test_browser_block_unknown_command() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let pkt = test_packet(4, CommandId::SpawnProcess, Payload::Empty);
        assert!(block.handle_message(&pkt).is_err());
    }

    #[test]
    fn test_browser_block_status() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let pkt = test_packet(
            4,
            CommandId::Custom,
            Payload::Custom("browser_status".into(), Vec::new()),
        );
        let resp = block.handle_message(&pkt).unwrap().unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
    }

    #[test]
    fn test_browser_block_empty_url_rejected() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let pkt = test_packet(
            4,
            CommandId::Custom,
            Payload::Custom("open_native".into(), Vec::new()),
        );
        assert!(block.handle_message(&pkt).is_err());
    }

    #[test]
    fn test_browser_block_state_roundtrip() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let state = block.extract_state().unwrap();
        block.restore_state(&state).unwrap();
        assert_eq!(block.state(), BlockState::Active);
    }

    #[tokio::test]
    async fn test_browser_block_block_on_from_runtime() {
        let block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let result = block.block_on(block.engine().navigate("http://invalid.nonexistent.domain"));
        assert!(result.is_err());
    }
}
