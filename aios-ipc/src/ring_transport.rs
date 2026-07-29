use aios_core::error::{AIOSException, Result};
use aios_core::ipc_protocol::IpcPacket;
use aios_ringbuf::{RingBuffer, RingBufferConfig};
use std::collections::HashMap;
use std::sync::Mutex;

pub const DEFAULT_RING_CAPACITY: usize = 256 * 1024;
pub const HEAVY_PAYLOAD_THRESHOLD: usize = 4096;

pub struct RingBufferTransport {
    rings: HashMap<(u32, u32), Mutex<RingBuffer>>,
    ring_capacity: usize,
    metrics: RingMetrics,
}

#[derive(Debug, Clone, Default)]
pub struct RingMetrics {
    pub ring_sends: u64,
    pub ring_receives: u64,
    pub ring_full_rejections: u64,
}

impl RingBufferTransport {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_RING_CAPACITY)
    }

    pub fn with_capacity(ring_capacity: usize) -> Self {
        Self {
            rings: HashMap::new(),
            ring_capacity,
            metrics: RingMetrics::default(),
        }
    }

    pub fn send_via_ring(&mut self, packet: &IpcPacket) -> Result<bool> {
        let payload_bytes = packet.payload.to_bytes();
        if payload_bytes.len() < HEAVY_PAYLOAD_THRESHOLD {
            return Ok(false);
        }

        let serialized = packet.serialize().map_err(|e| {
            AIOSException::SerializationError(format!("Ring send serialize failed: {}", e))
        })?;

        let ring = self
            .rings
            .entry((packet.header.source_block, packet.header.target_block))
            .or_insert_with(|| {
                let config = RingBufferConfig {
                    capacity: self.ring_capacity,
                    zero_copy: true,
                };
                Mutex::new(RingBuffer::new(config).expect("Failed to create ring buffer"))
            });

        match ring.get_mut().unwrap().write(&serialized) {
            Ok(written) => {
                self.metrics.ring_sends += 1;
                log::trace!(
                    "RingTransport: {} → {} ({} bytes via ring)",
                    packet.header.source_block,
                    packet.header.target_block,
                    written
                );
                Ok(true)
            }
            Err(_) => {
                self.metrics.ring_full_rejections += 1;
                Ok(false)
            }
        }
    }

    pub fn try_receive_from_ring(&mut self, from: u32, to: u32) -> Option<IpcPacket> {
        let ring = self.rings.get(&(from, to))?;
        let ring = ring.lock().unwrap();

        let available = ring.available_read();
        if available == 0 {
            return None;
        }

        let mut buf = vec![0u8; available];
        match ring.read(&mut buf) {
            Ok(bytes_read) if bytes_read > 0 => {
                self.metrics.ring_receives += 1;
                match IpcPacket::deserialize(&buf[..bytes_read]) {
                    Ok(packet) => Some(packet),
                    Err(e) => {
                        log::error!("RingTransport: deserialization failed: {}", e);
                        None
                    }
                }
            }
            _ => None,
        }
    }

    pub fn ring_usage(&self, from: u32, to: u32) -> Option<f32> {
        self.rings
            .get(&(from, to))
            .map(|ring| ring.lock().unwrap().fill_ratio())
    }

    pub fn metrics(&self) -> &RingMetrics {
        &self.metrics
    }

    pub fn reset_metrics(&mut self) {
        self.metrics = RingMetrics::default();
    }

    pub fn active_rings(&self) -> Vec<(u32, u32)> {
        self.rings.keys().copied().collect()
    }

    pub fn ring_count(&self) -> usize {
        self.rings.len()
    }
}

impl Default for RingBufferTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::ipc_protocol::{CommandId, Payload};

    fn test_packet(source: u32, target: u32, size: usize) -> IpcPacket {
        IpcPacket::new(
            source,
            target,
            CommandId::Custom,
            Payload::Binary(vec![0xAB; size]),
        )
    }

    #[test]
    fn test_small_payload_skips_ring() {
        let mut transport = RingBufferTransport::new();
        let packet = test_packet(1, 2, 100);
        let used_ring = transport.send_via_ring(&packet).unwrap();
        assert!(!used_ring);
    }

    #[test]
    fn test_heavy_payload_uses_ring() {
        let mut transport = RingBufferTransport::new();
        let packet = test_packet(1, 2, 8192);
        let used_ring = transport.send_via_ring(&packet).unwrap();
        assert!(used_ring);
        assert_eq!(transport.metrics().ring_sends, 1);
    }

    #[test]
    fn test_ring_roundtrip() {
        let mut transport = RingBufferTransport::new();
        let packet = test_packet(1, 2, 8192);
        transport.send_via_ring(&packet).unwrap();

        let received = transport.try_receive_from_ring(1, 2).unwrap();
        assert_eq!(received.header.source_block, 1);
        assert_eq!(received.header.target_block, 2);
        assert_eq!(transport.metrics().ring_receives, 1);
    }

    #[test]
    fn test_ring_empty_no_receive() {
        let mut transport = RingBufferTransport::new();
        let result = transport.try_receive_from_ring(1, 2);
        assert!(result.is_none());
    }

    #[test]
    fn test_ring_usage() {
        let mut transport = RingBufferTransport::new();
        let packet = test_packet(1, 2, 8192);
        transport.send_via_ring(&packet).unwrap();

        let usage = transport.ring_usage(1, 2).unwrap();
        assert!(usage > 0.0);
    }

    #[test]
    fn test_active_rings() {
        let mut transport = RingBufferTransport::new();
        transport.send_via_ring(&test_packet(1, 2, 8192)).unwrap();
        transport.send_via_ring(&test_packet(3, 4, 8192)).unwrap();

        let mut rings = transport.active_rings();
        rings.sort();
        assert_eq!(rings, vec![(1, 2), (3, 4)]);
    }

    #[test]
    fn test_separate_rings_per_pair() {
        let mut transport = RingBufferTransport::new();
        transport.send_via_ring(&test_packet(1, 2, 8192)).unwrap();
        transport.send_via_ring(&test_packet(3, 4, 8192)).unwrap();

        let p1 = transport.try_receive_from_ring(1, 2).unwrap();
        assert_eq!(p1.header.source_block, 1);

        let p2 = transport.try_receive_from_ring(3, 4).unwrap();
        assert_eq!(p2.header.source_block, 3);

        assert!(transport.try_receive_from_ring(1, 2).is_none());
        assert!(transport.try_receive_from_ring(3, 4).is_none());
    }

    #[test]
    fn test_metrics_reset() {
        let mut transport = RingBufferTransport::new();
        transport.send_via_ring(&test_packet(1, 2, 8192)).unwrap();
        assert_eq!(transport.metrics().ring_sends, 1);
        transport.reset_metrics();
        assert_eq!(transport.metrics().ring_sends, 0);
    }

    #[test]
    fn test_ring_count() {
        let mut transport = RingBufferTransport::new();
        transport.send_via_ring(&test_packet(1, 2, 8192)).unwrap();
        transport.send_via_ring(&test_packet(2, 3, 8192)).unwrap();
        assert_eq!(transport.ring_count(), 2);
    }

    #[test]
    fn test_large_payload_roundtrip() {
        let mut transport = RingBufferTransport::new();
        let payload = vec![0x42u8; 100_000];
        let packet = IpcPacket::new(5, 10, CommandId::Custom, Payload::Binary(payload.clone()));
        transport.send_via_ring(&packet).unwrap();

        let received = transport.try_receive_from_ring(5, 10).unwrap();
        if let Payload::Binary(data) = received.payload {
            assert_eq!(data, payload);
        } else {
            panic!("Expected Binary payload");
        }
    }
}
