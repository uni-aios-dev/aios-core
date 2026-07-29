use crate::error::Result;
use crate::ipc_protocol::IpcPacket;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockId(pub u32);

impl BlockId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block_{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockManifest {
    pub id: BlockId,
    pub name: String,
    pub version: String,
    pub sha256: [u8; 32],
}

impl fmt::Display for BlockManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Block({}, {} v{})", self.id, self.name, self.version)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockState {
    Unloaded,
    Loaded,
    Active,
    Frozen,
    Error,
}

pub trait StatefulBlock: Send {
    fn id(&self) -> BlockId;
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn state(&self) -> BlockState;

    fn handle_message(&mut self, packet: &IpcPacket) -> Result<Option<IpcPacket>>;

    fn extract_state(&self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn restore_state(&mut self, _state: &[u8]) -> Result<()> {
        Ok(())
    }

    fn health_check(&self) -> bool {
        true
    }
}
