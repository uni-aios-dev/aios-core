//! Wire protocol shared between cluster nodes.
//!
//! Messages are serialized with `bincode` (compact, self-describing for this
//! fixed schema) and framed on the TCP transport as `[u32 LE length][payload]`.
use crate::types::*;
use serde::{Deserialize, Serialize};

/// One logical cluster exchange. Requests carry `request_id` and `from` so the
/// receiver can reply to the origin node and the caller can match acks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClusterMessage {
    /// Periodic announce; carries full [`NodeInfo`] for discovery.
    Hello(NodeInfo),
    /// Load snapshot of a node.
    Metrics { id: NodeId, metrics: NodeMetrics },
    /// Ask a node to run a process.
    Spawn {
        request_id: u64,
        from: String,
        spec: RemoteProcessSpec,
        /// Optional process state snapshot to restore after spawn (migration).
        state: Option<Vec<u8>>,
    },
    /// Result of a spawn request.
    SpawnAck {
        request_id: u64,
        pid: u64,
        ok: bool,
        error: Option<String>,
    },
    /// Ask a node to terminate a process.
    Kill {
        request_id: u64,
        from: String,
        pid: u64,
    },
    /// Result of a kill request.
    KillAck {
        request_id: u64,
        ok: bool,
        error: Option<String>,
    },
    /// Ask a node to change a process priority.
    SetPriority {
        request_id: u64,
        from: String,
        pid: u64,
        priority: u8,
    },
    /// Result of a priority change.
    SetPriorityAck {
        request_id: u64,
        ok: bool,
        error: Option<String>,
    },
    /// Ask a node for the persisted state snapshot of process `pid`.
    GetState {
        request_id: u64,
        from: String,
        pid: u64,
    },
    /// Reply carrying the process state snapshot.
    GetStateReply {
        request_id: u64,
        ok: bool,
        state: Vec<u8>,
        error: Option<String>,
    },
    /// Ask a node for its hosted process list.
    StatusRequest { from: String },
    /// Process list of a node.
    StatusReply { processes: Vec<RemoteProcessStatus> },
}

/// Encode a message as `[u32 LE length][bincode payload]`.
pub fn encode(msg: &ClusterMessage) -> Result<Vec<u8>, String> {
    let body = bincode::serialize(msg).map_err(|e| format!("serialize: {e}"))?;
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Decode the first frame from `data`, returning the message and how many
/// bytes it consumed. Errors when the buffer holds an incomplete frame.
pub fn decode_frame(data: &[u8]) -> Result<(ClusterMessage, usize), String> {
    if data.len() < 4 {
        return Err("incomplete frame header".to_string());
    }
    let len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if data.len() < 4 + len {
        return Err("incomplete frame body".to_string());
    }
    let msg: ClusterMessage =
        bincode::deserialize(&data[4..4 + len]).map_err(|e| format!("deserialize: {e}"))?;
    Ok((msg, 4 + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_info(id: NodeId, addr: &str) -> NodeInfo {
        NodeInfo {
            id,
            name: format!("node-{id}"),
            addr: addr.to_string(),
            tier: 2,
            status: NodeStatus::Online,
            metrics: NodeMetrics::idle(),
        }
    }

    #[test]
    fn test_hello_roundtrip() {
        let msg = ClusterMessage::Hello(sample_info(7, "10.0.0.7:9000"));
        let frame = encode(&msg).unwrap();
        let (decoded, used) = decode_frame(&frame).unwrap();
        assert_eq!(used, frame.len());
        match decoded {
            ClusterMessage::Hello(info) => {
                assert_eq!(info.id, 7);
                assert_eq!(info.addr, "10.0.0.7:9000");
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn test_spawn_roundtrip() {
        let spec = RemoteProcessSpec::new("net", 2, 256).with_block_id(42);
        let msg = ClusterMessage::Spawn {
            request_id: 9,
            from: "10.0.0.1:9000".into(),
            spec,
            state: Some(vec![1, 2, 3]),
        };
        let frame = encode(&msg).unwrap();
        let (decoded, _) = decode_frame(&frame).unwrap();
        match decoded {
            ClusterMessage::Spawn {
                request_id,
                from,
                spec,
                state,
            } => {
                assert_eq!(request_id, 9);
                assert_eq!(from, "10.0.0.1:9000");
                assert_eq!(spec.name, "net");
                assert_eq!(spec.block_id, Some(42));
                assert_eq!(state, Some(vec![1, 2, 3]));
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn test_get_state_roundtrip() {
        let msg = ClusterMessage::GetState {
            request_id: 5,
            from: "10.0.0.1:9000".into(),
            pid: 7,
        };
        let frame = encode(&msg).unwrap();
        let (decoded, _) = decode_frame(&frame).unwrap();
        match decoded {
            ClusterMessage::GetState {
                request_id,
                from,
                pid,
            } => {
                assert_eq!(request_id, 5);
                assert_eq!(from, "10.0.0.1:9000");
                assert_eq!(pid, 7);
            }
            other => panic!("expected GetState, got {other:?}"),
        }

        let reply = ClusterMessage::GetStateReply {
            request_id: 5,
            ok: true,
            state: vec![9, 9, 9],
            error: None,
        };
        let frame = encode(&reply).unwrap();
        let (decoded, _) = decode_frame(&frame).unwrap();
        match decoded {
            ClusterMessage::GetStateReply {
                request_id,
                ok,
                state,
                error,
            } => {
                assert_eq!(request_id, 5);
                assert!(ok);
                assert_eq!(state, vec![9, 9, 9]);
                assert!(error.is_none());
            }
            other => panic!("expected GetStateReply, got {other:?}"),
        }
    }

    #[test]
    fn test_incomplete_frame_errors() {
        assert!(decode_frame(b"\x10").is_err());
        let msg = ClusterMessage::Metrics {
            id: 1,
            metrics: NodeMetrics::new(0.5, 512, 4096, 3),
        };
        let frame = encode(&msg).unwrap();
        assert!(decode_frame(&frame[..frame.len() - 2]).is_err());
    }

    #[test]
    fn test_multiple_frames_in_buffer() {
        let a = encode(&ClusterMessage::Metrics {
            id: 1,
            metrics: NodeMetrics::idle(),
        })
        .unwrap();
        let b = encode(&ClusterMessage::Hello(sample_info(2, "x:1"))).unwrap();
        let mut buf = a.clone();
        buf.extend_from_slice(&b);
        let (msg_a, used) = decode_frame(&buf).unwrap();
        assert_eq!(used, a.len());
        assert!(matches!(msg_a, ClusterMessage::Metrics { .. }));
        let (msg_b, used_b) = decode_frame(&buf[used..]).unwrap();
        assert_eq!(used_b, b.len());
        assert!(matches!(msg_b, ClusterMessage::Hello(_)));
    }
}
