use crate::session::{BrowserSession, SessionSnapshot};
use crate::types::{BrowserConfig, BrowserError, Page};
use aios_core::block::{BlockId, BlockState, StatefulBlock};
use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use std::future::Future;

/// First-class AIOS block that wraps [`BrowserSession`] (engine + tabs +
/// history + bookmarks) and exposes browsing through the kernel IPC channel.
///
/// The block drives the async navigation futures on a dedicated on-demand
/// Tokio runtime, so the synchronous [`StatefulBlock::handle_message`] never
/// blocks the caller's runtime and never panics on nested runtimes.
///
/// Custom commands: `browse`, `open_native`, `browser_status`,
/// `session_status`, `back`, `forward`, `reload`, `new_tab`, `close_tab`,
/// `select_tab`, `add_bookmark`, `list_bookmarks`, `remove_bookmark`,
/// `eval_js`, `add_user_script`.
pub struct BrowserBlock {
    id: BlockId,
    session: BrowserSession,
    state: BlockState,
}

impl BrowserBlock {
    /// Creates a browser block with the given block id and browsing config.
    pub fn new(id: BlockId, config: BrowserConfig) -> Self {
        Self {
            id,
            session: BrowserSession::new(config),
            state: BlockState::Active,
        }
    }

    /// Returns a reference to the underlying browsing session.
    pub fn session(&self) -> &BrowserSession {
        &self.session
    }

    /// Returns a reference to the underlying browser engine.
    pub fn engine(&self) -> &crate::engine::BrowserEngine {
        self.session.engine()
    }

    /// Runs an async future to completion from a synchronous context.
    ///
    /// If the current thread already runs inside a Tokio runtime, the future
    /// is driven on a dedicated OS thread (runtime created and dropped there)
    /// to avoid the nested-runtime panic.
    fn block_on<F>(future: F) -> F::Output
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

    /// Fetches and parses a page through the active session tab.
    fn browse(&mut self, url: &str) -> std::result::Result<Page, BrowserError> {
        Self::block_on(self.session.go(url))
    }

    /// Serializes `page` as a binary success response to `packet`.
    fn page_response(&self, packet: &IpcPacket, page: &Page) -> Result<Option<IpcPacket>> {
        let bytes = bincode::serialize(page)
            .map_err(|e| AIOSException::SerializationError(e.to_string()))?;
        Ok(Some(IpcPacket::response_ok(
            self.id.0,
            packet.header.source_block,
            packet.header.packet_id,
            Payload::Binary(bytes),
        )))
    }

    /// Builds a text success response carrying `text`.
    fn text_response(&self, packet: &IpcPacket, text: String) -> Option<IpcPacket> {
        Some(IpcPacket::response_ok(
            self.id.0,
            packet.header.source_block,
            packet.header.packet_id,
            Payload::Text(text),
        ))
    }

    /// Parses a payload as a decimal `usize` index.
    fn parse_index(data: &[u8]) -> Result<usize> {
        let text = std::str::from_utf8(data)
            .map_err(|_| AIOSException::InvalidPayload("index is not utf-8".into()))?;
        text.trim()
            .parse::<usize>()
            .map_err(|_| AIOSException::InvalidPayload(format!("bad index: {text}")))
    }

    /// Handles a custom IPC command, returning an error for unknown commands.
    fn handle_custom(
        &mut self,
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
                    Ok(page) => self.page_response(packet, &page),
                    Err(e) => Err(AIOSException::Generic(e.to_string())),
                }
            }
            "back" | "forward" | "reload" => {
                let outcome: std::result::Result<Option<Page>, BrowserError> = if cmd_name == "back"
                {
                    Self::block_on(self.session.back())
                } else if cmd_name == "forward" {
                    Self::block_on(self.session.forward())
                } else {
                    Self::block_on(self.session.reload()).map(Some)
                };
                match outcome {
                    Ok(Some(page)) => self.page_response(packet, &page),
                    Ok(None) => Err(AIOSException::Generic(format!(
                        "browser has no {cmd_name} history"
                    ))),
                    Err(e) => Err(AIOSException::Generic(e.to_string())),
                }
            }
            "new_tab" => {
                let index = self.session.new_tab();
                Ok(self.text_response(packet, format!("{{\"tab\":{index}}}")))
            }
            "close_tab" => {
                let index = if data.is_empty() {
                    self.session.active_tab()
                } else {
                    Self::parse_index(data)?
                };
                self.session
                    .close_tab(index)
                    .map_err(AIOSException::Generic)?;
                Ok(self.text_response(packet, format!("closed tab {index}")))
            }
            "select_tab" => {
                let index = Self::parse_index(data)?;
                self.session
                    .select_tab(index)
                    .map_err(AIOSException::Generic)?;
                Ok(self.text_response(packet, format!("selected tab {index}")))
            }
            "add_bookmark" => {
                let input: serde_json::Value = serde_json::from_slice(data)
                    .map_err(|e| AIOSException::InvalidPayload(e.to_string()))?;
                let name = input["name"].as_str().unwrap_or("").to_string();
                let url = input["url"].as_str().unwrap_or("").to_string();
                if url.trim().is_empty() {
                    return Err(AIOSException::InvalidPayload(
                        "bookmark url required".into(),
                    ));
                }
                let index = self.session.add_bookmark(name, url);
                Ok(self.text_response(packet, format!("bookmark {index} saved")))
            }
            "list_bookmarks" => {
                let json = serde_json::to_string(self.session.bookmarks())
                    .map_err(|e| AIOSException::SerializationError(e.to_string()))?;
                Ok(self.text_response(packet, json))
            }
            "remove_bookmark" => {
                let index = Self::parse_index(data)?;
                let removed = self
                    .session
                    .remove_bookmark(index)
                    .ok_or_else(|| AIOSException::Generic(format!("no bookmark {index}")))?;
                Ok(self.text_response(packet, format!("removed {}", removed.url)))
            }
            "eval_js" => {
                let source = std::str::from_utf8(data)
                    .map_err(|_| AIOSException::InvalidPayload("source is not utf-8".into()))?;
                let (value, console) = self
                    .session
                    .eval_js(source)
                    .map_err(|e| AIOSException::Generic(format!("eval_js failed: {e}")))?;
                let value = serde_json::to_value(&value)
                    .map_err(|e| AIOSException::SerializationError(e.to_string()))?;
                let json = serde_json::json!({ "value": value, "console": console });
                Ok(self.text_response(packet, json.to_string()))
            }
            "add_user_script" => {
                let input: serde_json::Value = serde_json::from_slice(data)
                    .map_err(|e| AIOSException::InvalidPayload(e.to_string()))?;
                let pattern = input["pattern"].as_str().unwrap_or("*").to_string();
                let source = input["source"]
                    .as_str()
                    .ok_or_else(|| AIOSException::InvalidPayload("script source required".into()))?
                    .to_string();
                self.session.add_user_script(pattern.clone(), source);
                Ok(self.text_response(packet, format!("userscript installed for {pattern}")))
            }
            "session_status" | "browser_status" => {
                let config = self.session.engine().config();
                let status = serde_json::json!({
                    "name": "browser",
                    "version": self.version(),
                    "state": format!("{:?}", self.state),
                    "user_agent": config.user_agent,
                    "timeout_secs": config.timeout_secs,
                    "sandbox_enabled": config.sandbox_enabled,
                    "execute_scripts": config.execute_scripts,
                    "tabs": self.session.tab_count(),
                    "active_tab": self.session.active_tab(),
                    "can_back": self.session.can_back(),
                    "can_forward": self.session.can_forward(),
                    "current_url": self.session.current_url(),
                    "bookmarks": self.session.bookmarks().len(),
                });
                Ok(self.text_response(packet, status.to_string()))
            }
            "open_native" => {
                let url = String::from_utf8_lossy(data).to_string();
                if url.trim().is_empty() {
                    return Err(AIOSException::InvalidPayload("empty URL".into()));
                }
                match open::that(&url) {
                    Ok(()) => {
                        Ok(self.text_response(packet, format!("opened in native browser: {url}")))
                    }
                    Err(e) => Err(AIOSException::Generic(format!(
                        "failed to open native browser: {e}"
                    ))),
                }
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
        self.session.engine().config().timeout_secs > 0
    }

    fn extract_state(&self) -> Result<Vec<u8>> {
        let state = (
            self.session.engine().config().clone(),
            self.state,
            self.session.snapshot(),
        );
        bincode::serialize(&state).map_err(|e| AIOSException::StateExtractionFailed(e.to_string()))
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<()> {
        // Preferred format: (config, state, session snapshot).
        if let Ok((_, restored, snapshot)) =
            bincode::deserialize::<(BrowserConfig, BlockState, SessionSnapshot)>(state)
        {
            self.state = restored;
            self.session.restore(snapshot);
            return Ok(());
        }
        // Legacy format from older releases: (config, state) only.
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

    fn custom_packet(command: &str, data: Vec<u8>) -> IpcPacket {
        test_packet(4, CommandId::Custom, Payload::Custom(command.into(), data))
    }

    #[test]
    fn test_browser_block_creation() {
        let block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        assert_eq!(block.name(), "browser");
        assert_eq!(block.id(), BlockId::new(4));
        assert_eq!(block.state(), BlockState::Active);
        assert_eq!(block.session().tab_count(), 1);
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
    fn test_browser_block_status_reports_script_config() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let pkt = custom_packet("browser_status", Vec::new());
        let resp = block.handle_message(&pkt).unwrap().unwrap();
        let Payload::Text(text) = resp.payload else {
            panic!("expected text payload");
        };
        assert!(text.contains("\"execute_scripts\":true"), "status: {text}");
        assert!(text.contains("\"tabs\":1"), "status: {text}");
    }

    #[test]
    fn test_session_status_tracks_navigation_flags() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let pkt = custom_packet("session_status", Vec::new());
        let resp = block.handle_message(&pkt).unwrap().unwrap();
        let Payload::Text(text) = resp.payload else {
            panic!("expected text payload");
        };
        assert!(text.contains("\"can_back\":false"), "status: {text}");
        assert!(text.contains("\"current_url\":null"), "status: {text}");
    }

    #[test]
    fn test_browser_block_empty_url_rejected() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let pkt = custom_packet("open_native", Vec::new());
        assert!(block.handle_message(&pkt).is_err());
    }

    #[test]
    fn test_back_without_history_errors() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let pkt = custom_packet("back", Vec::new());
        assert!(block.handle_message(&pkt).is_err());
        let pkt = custom_packet("forward", Vec::new());
        assert!(block.handle_message(&pkt).is_err());
        let pkt = custom_packet("reload", Vec::new());
        assert!(block.handle_message(&pkt).is_err());
    }

    #[test]
    fn test_tab_commands_roundtrip() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let resp = block
            .handle_message(&custom_packet("new_tab", Vec::new()))
            .unwrap()
            .unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
        assert_eq!(block.session().tab_count(), 2);
        assert_eq!(block.session().active_tab(), 1);

        let resp = block
            .handle_message(&custom_packet("select_tab", b"0".to_vec()))
            .unwrap()
            .unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
        assert_eq!(block.session().active_tab(), 0);

        let resp = block
            .handle_message(&custom_packet("close_tab", b"1".to_vec()))
            .unwrap()
            .unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
        assert_eq!(block.session().tab_count(), 1);

        // Closing the last tab must fail.
        assert!(block
            .handle_message(&custom_packet("close_tab", Vec::new()))
            .is_err());
        // A bad index must fail.
        assert!(block
            .handle_message(&custom_packet("select_tab", b"9".to_vec()))
            .is_err());
    }

    #[test]
    fn test_bookmark_commands_roundtrip() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let payload = br#"{"name":"Home","url":"https://example.com/"}"#.to_vec();
        let resp = block
            .handle_message(&custom_packet("add_bookmark", payload))
            .unwrap()
            .unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));

        let resp = block
            .handle_message(&custom_packet("list_bookmarks", Vec::new()))
            .unwrap()
            .unwrap();
        let Payload::Text(list) = resp.payload else {
            panic!("expected text payload");
        };
        assert!(list.contains("https://example.com/"), "list: {list}");

        let resp = block
            .handle_message(&custom_packet("remove_bookmark", b"0".to_vec()))
            .unwrap()
            .unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
        assert!(block.session().bookmarks().is_empty());
        assert!(block
            .handle_message(&custom_packet("remove_bookmark", b"0".to_vec()))
            .is_err());

        // A bookmark without a URL must be rejected.
        let bad = br#"{"name":"NoUrl"}"#.to_vec();
        assert!(block
            .handle_message(&custom_packet("add_bookmark", bad))
            .is_err());
    }

    #[test]
    fn test_eval_js_without_page_errors() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let pkt = custom_packet("eval_js", b"1 + 1".to_vec());
        assert!(block.handle_message(&pkt).is_err());
    }

    #[test]
    fn test_add_user_script_accepts_payload() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let payload = br#"{"pattern":"*","source":"console.log('hi')"}"#.to_vec();
        let resp = block
            .handle_message(&custom_packet("add_user_script", payload))
            .unwrap()
            .unwrap();
        assert!(matches!(resp.payload, Payload::Text(_)));
        let bad = b"not-json".to_vec();
        assert!(block
            .handle_message(&custom_packet("add_user_script", bad))
            .is_err());
    }

    #[test]
    fn test_browser_block_state_roundtrip() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let state = block.extract_state().unwrap();
        block.restore_state(&state).unwrap();
        assert_eq!(block.state(), BlockState::Active);
        assert_eq!(block.session().tab_count(), 1);
    }

    #[test]
    fn test_restore_accepts_legacy_two_tuple_state() {
        let mut block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let legacy = bincode::serialize(&(BrowserConfig::default(), BlockState::Frozen)).unwrap();
        block.restore_state(&legacy).unwrap();
        assert_eq!(block.state(), BlockState::Frozen);
        // Legacy restore keeps the session usable with one tab.
        assert_eq!(block.session().tab_count(), 1);
    }

    #[tokio::test]
    async fn test_browser_block_block_on_from_runtime() {
        let block = BrowserBlock::new(BlockId::new(4), BrowserConfig::default());
        let result =
            BrowserBlock::block_on(block.engine().navigate("http://invalid.nonexistent.domain"));
        assert!(result.is_err());
    }
}
