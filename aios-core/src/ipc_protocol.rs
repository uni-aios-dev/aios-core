use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static PACKET_COUNTER: AtomicU64 = AtomicU64::new(1);

#[repr(C)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Header {
    pub packet_id: u64,
    pub source_block: u32,
    pub target_block: u32,
    pub command_id: u16,
    pub priority: u8,
    pub payload_len: u32,
    pub checksum: [u8; 32],
}

pub const HEADER_SIZE: usize = std::mem::size_of::<Header>();

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Payload {
    Empty,
    Binary(Vec<u8>),
    Text(String),
    RegisterBlock {
        name: String,
        version: String,
        binary: Vec<u8>,
    },
    UnloadBlock {
        block_id: u32,
    },
    GetTopology,
    SpawnProcess {
        name: String,
        priority: u8,
        ram_mb: u64,
    },
    KillProcess {
        pid: u64,
    },
    AdjustPriority {
        pid: u64,
        new_priority: u8,
    },
    HealthCheck,
    ExtractState,
    RestoreState(Vec<u8>),
    HotSwap {
        block_id: u32,
        new_binary: Vec<u8>,
        new_version: String,
    },
    Rollback {
        block_id: u32,
    },
    IntentCommand {
        intent: String,
        context: serde_json::Value,
    },
    Custom(String, Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Response {
    Success(Payload),
    Failure { code: u16, message: String },
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpcPacket {
    pub header: Header,
    pub payload: Payload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u16)]
pub enum CommandId {
    RegisterBlock = 0x0001,
    UnloadBlock = 0x0002,
    GetTopology = 0x0003,
    SpawnProcess = 0x0010,
    KillProcess = 0x0011,
    AdjustPriority = 0x0012,
    HealthCheck = 0x0020,
    ExtractState = 0x0030,
    RestoreState = 0x0031,
    HotSwap = 0x0040,
    Rollback = 0x0041,
    IntentCommand = 0x0050,
    Custom = 0x00FF,
}

impl IpcPacket {
    pub fn new(source_block: u32, target_block: u32, command: CommandId, payload: Payload) -> Self {
        let payload_bytes = payload.to_bytes();
        let checksum = crate::crypto::compute_sha256_bytes(&payload_bytes);

        Self {
            header: Header {
                packet_id: PACKET_COUNTER.fetch_add(1, Ordering::Relaxed),
                source_block,
                target_block,
                command_id: command as u16,
                priority: 2,
                payload_len: payload_bytes.len() as u32,
                checksum,
            },
            payload,
        }
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.header.priority = priority;
        self
    }

    pub fn response_ok(source: u32, target: u32, in_reply_to: u64, payload: Payload) -> Self {
        let payload_bytes = payload.to_bytes();
        let checksum = crate::crypto::compute_sha256_bytes(&payload_bytes);
        Self {
            header: Header {
                packet_id: in_reply_to.wrapping_add(1_000_000),
                source_block: source,
                target_block: target,
                command_id: CommandId::Custom as u16,
                priority: 2,
                payload_len: payload_bytes.len() as u32,
                checksum,
            },
            payload,
        }
    }

    pub fn response_err(source: u32, target: u32, in_reply_to: u64, _msg: String) -> Self {
        let payload = Payload::Empty;
        let payload_bytes = payload.to_bytes();
        let checksum = crate::crypto::compute_sha256_bytes(&payload_bytes);
        Self {
            header: Header {
                packet_id: in_reply_to.wrapping_add(2_000_000),
                source_block: source,
                target_block: target,
                command_id: CommandId::Custom as u16,
                priority: 0,
                payload_len: payload_bytes.len() as u32,
                checksum,
            },
            payload,
        }
    }

    pub fn serialize(&self) -> Result<Vec<u8>, Box<bincode::ErrorKind>> {
        bincode::serialize(self)
    }

    pub fn deserialize(data: &[u8]) -> Result<Self, Box<bincode::ErrorKind>> {
        bincode::deserialize(data)
    }

    pub fn verify_checksum(&self) -> bool {
        let payload_bytes = self.payload.to_bytes();
        let expected = crate::crypto::compute_sha256_bytes(&payload_bytes);
        self.header.checksum == expected
    }
}

impl Payload {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Payload::Empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_roundtrip() {
        let pkt = IpcPacket::new(
            0,
            1,
            CommandId::HealthCheck,
            Payload::Binary(b"ping".to_vec()),
        );
        let bytes = pkt.serialize().unwrap();
        let restored = IpcPacket::deserialize(&bytes).unwrap();
        assert_eq!(pkt.header.packet_id, restored.header.packet_id);
        assert_eq!(pkt.header.source_block, restored.header.source_block);
        assert_eq!(pkt.header.target_block, restored.header.target_block);
        assert_eq!(pkt.payload, restored.payload);
    }

    #[test]
    fn test_checksum_valid() {
        let pkt = IpcPacket::new(0, 1, CommandId::HealthCheck, Payload::Empty);
        assert!(pkt.verify_checksum());
    }

    #[test]
    fn test_checksum_detects_tamper() {
        let mut pkt = IpcPacket::new(
            0,
            1,
            CommandId::HealthCheck,
            Payload::Binary(b"data".to_vec()),
        );
        assert!(pkt.verify_checksum());
        pkt.payload = Payload::Binary(b"tampered".to_vec());
        assert!(!pkt.verify_checksum());
    }

    #[test]
    fn test_serialize_speed() {
        let pkt = IpcPacket::new(
            0,
            1,
            CommandId::SpawnProcess,
            Payload::SpawnProcess {
                name: "test_process".into(),
                priority: 2,
                ram_mb: 256,
            },
        );
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let _ = pkt.serialize().unwrap();
        }
        let elapsed = start.elapsed();
        let per_us = elapsed.as_micros() as f64 / 10_000.0;
        println!("Serialize: {per_us:.3} us/packet (10k iterations)");
        // Debug builds are ~10-20x slower due to lack of optimizations
        let threshold = if cfg!(debug_assertions) { 50.0 } else { 1.0 };
        assert!(
            per_us < threshold,
            "Serialization too slow: {per_us} us (threshold: {threshold})"
        );
    }

    #[test]
    fn test_deserialize_speed() {
        let pkt = IpcPacket::new(
            0,
            1,
            CommandId::SpawnProcess,
            Payload::SpawnProcess {
                name: "test_process".into(),
                priority: 2,
                ram_mb: 256,
            },
        );
        let bytes = pkt.serialize().unwrap();
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let _ = IpcPacket::deserialize(&bytes).unwrap();
        }
        let elapsed = start.elapsed();
        let per_us = elapsed.as_micros() as f64 / 10_000.0;
        println!("Deserialize: {per_us:.3} us/packet (10k iterations)");
        let threshold = if cfg!(debug_assertions) { 50.0 } else { 1.0 };
        assert!(
            per_us < threshold,
            "Deserialization too slow: {per_us} us (threshold: {threshold})"
        );
    }
}
